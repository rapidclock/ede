//! Sole owner of terminal modes and Unix signal subscriptions.

use std::{
    env,
    io::{self, IsTerminal},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use crossterm::{cursor, event, execute, terminal};
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
}

/// Production driver. Crossterm captures/restores the original termios state for raw mode.
pub(crate) struct UnixTerminal;

impl TerminalDriver for UnixTerminal {
    /// Issue each command independently, so one write failure does not skip later cleanup.
    fn set(&mut self, mode: Mode, enabled: bool) -> io::Result<()> {
        let mut out = io::stdout();
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
        }
    }
}

/// Guard tracking attempted acquisition, including writes that may fail after partial output.
pub(crate) struct TerminalSession<D: TerminalDriver> {
    driver: D,
    acquired: Vec<Mode>,
}

impl<D: TerminalDriver> TerminalSession<D> {
    /// Construct an inactive owner; construction itself has no terminal side effects.
    pub(crate) fn new(driver: D) -> Self {
        Self {
            driver,
            acquired: Vec::new(),
        }
    }

    /// Acquire resources once. On any failure the caller or Drop must restore the guard.
    pub(crate) fn acquire(&mut self) -> io::Result<()> {
        if !self.acquired.is_empty() {
            return Err(io::Error::other("terminal session is already acquired"));
        }
        for mode in MODES {
            self.acquired.push(mode);
            self.driver.set(mode, true)?;
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
        if !job_control_available() {
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
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other(
            "interactive mode requires terminal stdin and stdout; pipes are not supported yet",
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

/// A foreground job must have a controlling terminal and a parent shell session.
fn job_control_available() -> bool {
    getsid(None).is_ok_and(|session| session != getpid())
        && tcgetpgrp(io::stdin()).is_ok_and(|foreground| foreground == getpgrp())
        && getpgid(Some(getppid())).is_ok_and(|parent| parent != getpgrp())
}

/// Flags written by signal-safe handlers, consumed only by the application thread.
pub(crate) struct Signals {
    termination: Arc<AtomicUsize>,
    suspend: Arc<AtomicBool>,
    continued: Arc<AtomicBool>,
    registrations: Vec<SigId>,
}

impl Signals {
    /// Register early, before acquisition or any future background threads.
    pub(crate) fn register() -> io::Result<Self> {
        let mut signals = Self {
            termination: Arc::new(AtomicUsize::new(0)),
            suspend: Arc::new(AtomicBool::new(false)),
            continued: Arc::new(AtomicBool::new(false)),
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
        Ok(signals)
    }

    /// Termination takes precedence over suspension and ordinary input.
    pub(crate) fn termination(&self) -> Option<u8> {
        let signal = self.termination.swap(0, Ordering::Relaxed);
        (signal != 0).then_some(signal as u8)
    }

    /// Take a requested job-control suspension.
    pub(crate) fn suspend_requested(&self) -> bool {
        self.suspend.swap(false, Ordering::Relaxed)
    }

    /// Take a continuation notification, including an externally stopped process resuming.
    pub(crate) fn continued(&self) -> bool {
        self.continued.swap(false, Ordering::Relaxed)
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
