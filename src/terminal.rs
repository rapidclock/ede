//! Sole owner of terminal modes and Unix signal subscriptions.

use std::{
    cell::Cell,
    env,
    fs::File,
    io::{self, IsTerminal, Write},
    os::fd::{AsFd, AsRawFd},
    os::unix::fs::OpenOptionsExt,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crossterm::{cursor, event, execute, terminal};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{self, SetArg, Termios};
use nix::unistd::{getpgid, getpgrp, getpid, getppid, getsid, tcgetpgrp};
use signal_hook::{SigId, consts::signal::*, flag, low_level};

/// One acquired terminal resource, restored in reverse acquisition order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    Raw,
    AlternateScreen,
    HiddenCursor,
    Mouse,
    Paste,
    Focus,
    Keyboard,
}

/// Ordered acquisition; each operation has a corresponding independent restoration attempt.
const MODES: [Mode; 6] = [
    Mode::Raw,
    Mode::AlternateScreen,
    Mode::HiddenCursor,
    Mode::Mouse,
    Mode::Paste,
    Mode::Focus,
];

/// Small failure-injection seam around actual terminal side effects.
pub(crate) trait TerminalDriver {
    /// Enable or disable a single resource; disabling must be idempotent.
    fn set(&mut self, mode: Mode, enabled: bool) -> io::Result<()>;
    /// Ask for keyboard enhancement support without waiting for a reply.
    fn query_keyboard(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Check foreground ownership against the terminal used for input.
    fn foreground(&self) -> bool {
        false
    }
}

/// The controlling tty is explicit when fd 0 was supplied by a producer.
#[derive(Default)]
pub(crate) struct UnixTerminal {
    tty: Option<File>,
    raw: Option<Termios>,
    output_lost: Rc<Cell<bool>>,
}

/// None, all, or only part of a framed clipboard request reached the tty.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardWrite {
    Sent,
    Unavailable,
    Lost,
}

impl UnixTerminal {
    /// Validate the controlling terminal before touching the inherited data descriptor.
    pub(crate) fn controlling() -> io::Result<Self> {
        let tty = File::options().read(true).write(true).open("/dev/tty")?;
        if tcgetpgrp(&tty)? != getpgrp() {
            return Err(io::Error::other(
                "controlling terminal is not in the foreground",
            ));
        }
        termios::tcgetattr(&tty)?;
        size_of(&tty)?;
        Ok(Self {
            tty: Some(tty),
            raw: None,
            output_lost: Rc::default(),
        })
    }

    /// Independent CLOEXEC handle for the single terminal input decoder.
    pub(crate) fn input(&self) -> io::Result<Option<File>> {
        self.tty
            .as_ref()
            .map(|_| {
                File::options()
                    .read(true)
                    .custom_flags(nix::libc::O_NONBLOCK)
                    .open("/dev/tty")
            })
            .transpose()
    }

    /// Keep the normal stdout path and use the controlling tty in stream mode.
    pub(crate) fn output(&self) -> io::Result<TerminalOutput> {
        Ok(TerminalOutput {
            tty: self.tty.as_ref().map(File::try_clone).transpose()?,
            lost: self.output_lost.clone(),
        })
    }

    /// Obtain geometry from the same endpoint used for display and input.
    pub(crate) fn size(&self) -> io::Result<(u16, u16)> {
        self.tty.as_ref().map_or_else(terminal::size, size_of)
    }

    /// Reopen the renderer's tty without changing flags on its existing descriptor.
    fn clipboard_output(&self) -> io::Result<File> {
        let path = if self.tty.is_some() {
            std::path::PathBuf::from("/dev/tty")
        } else {
            nix::unistd::ttyname(io::stdout())?
        };
        let tty = File::options()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY)
            .open(path)?;
        if tcgetpgrp(&tty)? != getpgrp() {
            return Err(io::Error::other("clipboard tty is not in the foreground"));
        }
        Ok(tty)
    }
}

/// Keep the complete frame under one output owner, including short writes.
fn clipboard_frame(
    frame: &[u8],
    mut write: impl FnMut(&[u8]) -> io::Result<usize>,
) -> ClipboardWrite {
    let mut written = 0;
    while written < frame.len() {
        match write(&frame[written..]) {
            Ok(count) if count != 0 => written += count,
            _ if written == 0 => return ClipboardWrite::Unavailable,
            _ => return ClipboardWrite::Lost,
        }
    }
    ClipboardWrite::Sent
}

/// Output destination shared by terminal commands and the rendering backend.
pub(crate) struct TerminalOutput {
    tty: Option<File>,
    lost: Rc<Cell<bool>>,
}

impl Write for TerminalOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.lost.get() {
            return Ok(buf.len());
        }
        match &mut self.tty {
            Some(out) => out.write(buf),
            None => io::stdout().write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.lost.get() {
            return Ok(());
        }
        match &mut self.tty {
            Some(out) => out.flush(),
            None => io::stdout().flush(),
        }
    }
}

/// Kernel geometry is local to this tty and requires no decoder round trip.
pub(crate) fn size_of(tty: &impl AsRawFd) -> io::Result<(u16, u16)> {
    use nix::libc;
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the descriptor is borrowed and the kernel writes a winsize to live storage.
    if unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ as _, &mut size) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if size.ws_col == 0 || size.ws_row == 0 {
        return Err(io::Error::other("terminal has no usable cell dimensions"));
    }
    Ok((size.ws_col, size.ws_row))
}

impl TerminalDriver for UnixTerminal {
    /// Issue each command independently, so one write failure does not skip later cleanup.
    fn set(&mut self, mode: Mode, enabled: bool) -> io::Result<()> {
        if self.output_lost.get() && mode != Mode::Raw {
            return Ok(());
        }
        if mode == Mode::Raw
            && let Some(tty) = &self.tty
        {
            if enabled {
                let original = termios::tcgetattr(tty)?;
                let mut raw = original.clone();
                termios::cfmakeraw(&mut raw);
                self.raw = Some(original);
                termios::tcsetattr(tty, SetArg::TCSANOW, &raw)?;
            } else if let Some(original) = &self.raw {
                termios::tcsetattr(tty, SetArg::TCSANOW, original)?;
                self.raw = None;
            }
            return Ok(());
        }
        let mut out = self.output()?;
        match (mode, enabled) {
            (Mode::Raw, true) => terminal::enable_raw_mode(),
            (Mode::Raw, false) => terminal::disable_raw_mode(),
            (Mode::AlternateScreen, true) => execute!(out, terminal::EnterAlternateScreen),
            (Mode::AlternateScreen, false) => execute!(out, terminal::LeaveAlternateScreen),
            (Mode::HiddenCursor, true) => execute!(out, cursor::Hide),
            (Mode::HiddenCursor, false) => execute!(out, cursor::Show),
            (Mode::Mouse, true) => execute!(out, event::EnableMouseCapture),
            (Mode::Mouse, false) => execute!(out, event::DisableMouseCapture),
            (Mode::Paste, true) => execute!(out, event::EnableBracketedPaste),
            (Mode::Paste, false) => execute!(out, event::DisableBracketedPaste),
            (Mode::Focus, true) => execute!(out, event::EnableFocusChange),
            (Mode::Focus, false) => execute!(out, event::DisableFocusChange),
            (Mode::Keyboard, true) => execute!(
                out,
                event::PushKeyboardEnhancementFlags(
                    event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                )
            ),
            (Mode::Keyboard, false) => execute!(out, event::PopKeyboardEnhancementFlags),
        }
    }
    /// Query only; the sole input reader consumes any response asynchronously.
    fn query_keyboard(&mut self) -> io::Result<()> {
        let mut output = self.output()?;
        output.write_all(b"\x1b[?u")?;
        output.flush()
    }
    fn foreground(&self) -> bool {
        let group = self
            .tty
            .as_ref()
            .map_or_else(|| tcgetpgrp(io::stdin()), tcgetpgrp);
        getsid(None).is_ok_and(|session| session != getpid())
            && group.is_ok_and(|foreground| foreground == getpgrp())
            && getpgid(Some(getppid())).is_ok_and(|parent| parent != getpgrp())
    }
}

/// Guard tracking attempted acquisition, including writes that may fail after partial output.
pub(crate) struct TerminalSession<D: TerminalDriver> {
    driver: D,
    acquired: Vec<Mode>,
    mouse: bool,
    keyboard: bool,
}

impl TerminalSession<UnixTerminal> {
    /// Send a prepared frame between renders, with a finite tty output deadline.
    pub(crate) fn clipboard(
        &mut self,
        transfer: &crate::editor::ClipboardTransfer,
        interrupted: impl Fn() -> bool,
    ) -> ClipboardWrite {
        let Ok(mut tty) = self.driver.clipboard_output() else {
            return ClipboardWrite::Unavailable;
        };
        let mut deadline = Instant::now() + Duration::from_millis(25);
        let mut started = false;
        self.write_clipboard(&transfer.frame, |remaining| {
            while let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                let result = if started {
                    tty.write(remaining)
                } else if interrupted() {
                    return Err(io::ErrorKind::Interrupted.into());
                } else {
                    transfer.write_first(|bytes| tty.write(bytes))
                };
                match result {
                    Ok(count) if count > 0 && !started => {
                        // Before admission we can skip a busy tty. After a short write,
                        // the client needs time to drain the frame before output resumes.
                        started = true;
                        deadline = Instant::now() + Duration::from_secs(1);
                        return Ok(count);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        let mut fd = [PollFd::new(tty.as_fd(), PollFlags::POLLOUT)];
                        let timeout = PollTimeout::try_from(wait).unwrap_or(PollTimeout::ZERO);
                        match poll(&mut fd, timeout) {
                            Ok(0) => break,
                            Ok(_) | Err(nix::errno::Errno::EINTR) => continue,
                            Err(error) => return Err(error.into()),
                        }
                    }
                    result => return result,
                }
            }
            Err(io::Error::from(io::ErrorKind::TimedOut))
        })
    }

    fn write_clipboard(
        &mut self,
        frame: &[u8],
        write: impl FnMut(&[u8]) -> io::Result<usize>,
    ) -> ClipboardWrite {
        let outcome = clipboard_frame(frame, write);
        if outcome == ClipboardWrite::Lost {
            // The client may still be parsing OSC. Restore termios, but send no more frames.
            self.driver.output_lost.set(true);
        }
        outcome
    }

    /// An incomplete control sequence prevents further terminal text and mode commands.
    pub(crate) fn output_lost(&self) -> bool {
        self.driver.output_lost.get()
    }
}

impl<D: TerminalDriver> TerminalSession<D> {
    /// Borrow the device to construct the renderer and ingress handles.
    pub(crate) fn driver(&self) -> &D {
        &self.driver
    }
    /// Construct an inactive owner; construction itself has no terminal side effects.
    pub(crate) fn new(driver: D) -> Self {
        Self {
            driver,
            acquired: Vec::new(),
            mouse: true,
            keyboard: false,
        }
    }

    /// Acquire resources once. On any failure the caller or Drop must restore the guard.
    pub(crate) fn acquire(&mut self) -> io::Result<()> {
        if !self.acquired.is_empty() {
            return Err(io::Error::other("terminal session is already acquired"));
        }
        for mode in MODES {
            if mode == Mode::Mouse && !self.mouse {
                continue;
            }
            self.acquired.push(mode);
            self.driver.set(mode, true)?;
        }
        if self.keyboard {
            self.set_keyboard()?;
        }
        self.driver.query_keyboard()?;
        Ok(())
    }
    /// Acquire enhanced keyboard state only after a timely positive capability reply.
    pub(crate) fn set_keyboard(&mut self) -> io::Result<()> {
        self.keyboard = true;
        if !self.acquired.contains(&Mode::Keyboard) {
            self.acquired.push(Mode::Keyboard);
            self.driver.set(Mode::Keyboard, true)?;
        }
        Ok(())
    }
    /// Pause application mouse capture and preserve that preference across suspension.
    pub(crate) fn set_mouse(&mut self, enabled: bool) -> io::Result<()> {
        self.mouse = enabled;
        if enabled && !self.acquired.contains(&Mode::Mouse) {
            self.acquired.push(Mode::Mouse);
            self.driver.set(Mode::Mouse, true)?;
        } else if !enabled
            && let Some(index) = self.acquired.iter().position(|mode| *mode == Mode::Mouse)
        {
            self.driver.set(Mode::Mouse, false)?;
            self.acquired.remove(index);
        }
        Ok(())
    }

    /// Attempt every acquired restoration; preserve failed resources for a later retry.
    pub(crate) fn restore(&mut self) -> io::Result<()> {
        let mut errors = Vec::new();
        for index in (0..self.acquired.len()).rev() {
            let mode = self.acquired[index];
            match self.driver.set(mode, false) {
                Ok(()) => {
                    self.acquired.remove(index);
                }
                Err(error) => errors.push(format!("{mode:?}: {error}")),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(errors.join("; ")))
        }
    }

    /// Return to the shell before stopping. Reacquire only after SIGCONT resumes execution.
    pub(crate) fn suspend(&mut self) -> io::Result<bool> {
        if !self.driver.foreground() {
            return Ok(false);
        }
        self.restore()?;
        low_level::raise(SIGSTOP)?;
        self.acquire()?;
        Ok(true)
    }
}

impl<D: TerminalDriver> Drop for TerminalSession<D> {
    /// Best-effort fallback for unwinding; explicit restoration reports errors to the caller.
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Validate without reading input, writing escapes, or changing terminal state.
pub(crate) fn validate() -> io::Result<()> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(
            "interactive mode requires terminal stdin and stdout",
        ));
    }
    validate_output()
}

/// Check stdout and TERM before opening any private or terminal state.
pub(crate) fn validate_output() -> io::Result<()> {
    if !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "interactive mode requires terminal stdout",
        ));
    }
    if env::var("TERM")
        .ok()
        .is_none_or(|term| term.trim().is_empty() || term.eq_ignore_ascii_case("dumb"))
    {
        return Err(io::Error::other(
            "a usable TERM value is required (not missing or dumb)",
        ));
    }
    Ok(())
}

/// Flags written by signal-safe handlers, consumed only by the application thread.
pub(crate) struct Signals {
    termination: Arc<AtomicUsize>,
    suspend: Arc<AtomicBool>,
    continued: Arc<AtomicBool>,
    cancel_paste: Arc<AtomicBool>,
    registrations: Vec<SigId>,
}

impl Signals {
    /// Register early, before acquisition or any future background threads.
    pub(crate) fn register() -> io::Result<Self> {
        let mut signals = Self {
            termination: Arc::new(AtomicUsize::new(0)),
            suspend: Arc::new(AtomicBool::new(false)),
            continued: Arc::new(AtomicBool::new(false)),
            cancel_paste: Arc::new(AtomicBool::new(false)),
            registrations: Vec::new(),
        };
        for signal in [SIGINT, SIGTERM, SIGHUP] {
            signals.registrations.push(flag::register_usize(
                signal,
                signals.termination.clone(),
                signal as usize,
            )?);
        }
        signals
            .registrations
            .push(flag::register(SIGTSTP, signals.suspend.clone())?);
        signals
            .registrations
            .push(flag::register(SIGCONT, signals.continued.clone())?);
        signals
            .registrations
            .push(flag::register(SIGUSR1, signals.cancel_paste.clone())?);
        Ok(signals)
    }

    /// Termination takes precedence over suspension and ordinary input.
    pub(crate) fn termination(&self) -> Option<u8> {
        let signal = self.termination.swap(0, Ordering::Relaxed);
        (signal != 0).then_some(signal as u8)
    }

    /// Defer an unstarted clipboard frame to terminal lifecycle requests.
    pub(crate) fn output_interrupted(&self) -> bool {
        self.termination.load(Ordering::Relaxed) != 0
            || self.suspend.load(Ordering::Relaxed)
            || self.continued.load(Ordering::Relaxed)
    }

    /// Take a requested job-control suspension.
    pub(crate) fn suspend_requested(&self) -> bool {
        self.suspend.swap(false, Ordering::Relaxed)
    }

    /// Take a continuation notification, including an externally stopped process resuming.
    pub(crate) fn continued(&self) -> bool {
        self.continued.swap(false, Ordering::Relaxed)
    }
    /// Take an out-of-band paste cancellation without decoding terminal payload bytes.
    pub(crate) fn cancel_paste_requested(&self) -> bool {
        self.cancel_paste.swap(false, Ordering::Relaxed)
    }
}

impl Drop for Signals {
    /// Unsubscribe our flags without altering another library's subscriptions.
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            low_level::unregister(registration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    /// A partial OSC cannot be followed by cursor cleanup, redraw, or late tty writes.
    #[test]
    fn incomplete_clipboard_frame_poisoned_output_includes_renderer_drop() {
        use ratatui::{Terminal, backend::CrosstermBackend};
        use std::os::unix::fs::FileExt;

        let sink = tempfile::tempfile().unwrap();
        let mut writer = sink.try_clone().unwrap();
        let mut session = TerminalSession::new(UnixTerminal {
            tty: Some(sink.try_clone().unwrap()),
            ..UnixTerminal::default()
        });
        let mut output = session.driver().output().unwrap();
        let mut renderer = Terminal::with_options(
            CrosstermBackend::new(session.driver().output().unwrap()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 40, 8)),
            },
        )
        .unwrap();
        renderer.hide_cursor().unwrap();
        assert_eq!(
            session.write_clipboard(b"\x1b]52;c;YWJj\x1b\\", |bytes| {
                if bytes.starts_with(b"\x1b]52;") {
                    writer.write(&bytes[..5])
                } else {
                    Err(io::ErrorKind::BrokenPipe.into())
                }
            }),
            ClipboardWrite::Lost
        );
        let written = sink.metadata().unwrap().len();
        drop(renderer);
        output.write_all(b"never sent").unwrap();
        assert_eq!(
            sink.metadata().unwrap().len(),
            written,
            "poisoned tty emitted cleanup bytes"
        );
        let mut tail = [0; 5];
        sink.read_exact_at(&mut tail, written - 5).unwrap();
        assert_eq!(&tail, b"\x1b]52;");
    }

    /// Shared trace survives Drop so tests can inspect unwind restoration.
    type Trace = Rc<RefCell<Vec<(Mode, bool)>>>;

    /// Inject a single setup or cleanup failure after recording the attempted side effect.
    struct Driver {
        trace: Trace,
        fail: Option<(Mode, bool)>,
    }

    impl TerminalDriver for Driver {
        /// Simulate partially applied operations as well as successful ones.
        fn set(&mut self, mode: Mode, enabled: bool) -> io::Result<()> {
            self.trace.borrow_mut().push((mode, enabled));
            if self.fail == Some((mode, enabled)) {
                self.fail = None;
                Err(io::Error::other("injected failure"))
            } else {
                Ok(())
            }
        }
    }

    /// Every failed acquisition rolls back even its possibly partial final operation.
    #[test]
    fn partial_setup_and_idempotent_cleanup() {
        for (index, mode) in MODES.iter().enumerate() {
            let trace = Trace::default();
            let mut session = TerminalSession::new(Driver {
                trace: trace.clone(),
                fail: Some((*mode, true)),
            });
            assert!(session.acquire().is_err());
            session.restore().unwrap();
            let count = trace.borrow().len();
            session.restore().unwrap();
            assert_eq!(trace.borrow().len(), count);
            let restored: Vec<_> = trace
                .borrow()
                .iter()
                .filter(|(_, enabled)| !enabled)
                .map(|(mode, _)| *mode)
                .collect();
            assert_eq!(
                restored,
                MODES[..=index].iter().rev().copied().collect::<Vec<_>>()
            );
        }
    }

    /// A cleanup error must not prevent restoring raw mode and other later resources.
    #[test]
    fn cleanup_continues_and_retries_failed_resource() {
        let trace = Trace::default();
        let mut session = TerminalSession::new(Driver {
            trace: trace.clone(),
            fail: Some((Mode::Mouse, false)),
        });
        session.acquire().unwrap();
        assert!(session.restore().is_err());
        assert!(trace.borrow().contains(&(Mode::Raw, false)));
        assert_eq!(session.acquired, [Mode::Mouse]);
        session.restore().unwrap();
        assert!(session.acquired.is_empty());
    }

    /// Dynamic keyboard/capture resources restore independently and preserve user choices on re-entry.
    #[test]
    fn enhanced_modes_and_paused_capture_survive_reentry() {
        let trace = Trace::default();
        let mut session = TerminalSession::new(Driver {
            trace: trace.clone(),
            fail: None,
        });
        session.acquire().unwrap();
        session.set_keyboard().unwrap();
        session.set_mouse(false).unwrap();
        session.restore().unwrap();
        let before = trace.borrow().len();
        session.acquire().unwrap();
        assert!(trace.borrow()[before..].contains(&(Mode::Keyboard, true)));
        assert!(!trace.borrow()[before..].contains(&(Mode::Mouse, true)));
        session.restore().unwrap();
        assert_eq!(
            trace
                .borrow()
                .iter()
                .filter(|entry| **entry == (Mode::Keyboard, false))
                .count(),
            2
        );
        let mut session = TerminalSession::new(Driver {
            trace: trace.clone(),
            fail: Some((Mode::Keyboard, true)),
        });
        session.acquire().unwrap();
        assert!(session.set_keyboard().is_err());
        session.restore().unwrap();
        assert!(session.acquired.is_empty());
    }

    /// Read/draw failures return through the guard, while unwinding uses the same Drop fallback.
    #[test]
    fn errors_and_unwind_restore_every_resource() {
        for panic in [false, true] {
            let trace = Trace::default();
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
                    let mut session = TerminalSession::new(Driver {
                        trace: trace.clone(),
                        fail: None,
                    });
                    session.acquire()?;
                    if panic {
                        panic!("injected draw panic");
                    }
                    Err(io::Error::other("injected read error"))
                }));
            assert!(result.is_err() || result.unwrap().is_err());
            let restored: Vec<_> = trace
                .borrow()
                .iter()
                .filter(|(_, enabled)| !enabled)
                .map(|(mode, _)| *mode)
                .collect();
            assert_eq!(restored, MODES.into_iter().rev().collect::<Vec<_>>());
        }
    }
}
