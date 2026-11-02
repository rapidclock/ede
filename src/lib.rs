//! Terminal foundation for ede. Application state is independent of terminal ownership.

mod app;
mod command;
mod input;
mod runtime;
pub mod status;
mod terminal;
mod ui;

use std::{
    fmt, io,
    panic::{self, AssertUnwindSafe},
};

use status::StatusInbox;
use terminal::{Signals, TerminalDriver, TerminalSession, UnixTerminal};

/// Outcome of an interactive session, including externally requested termination.
pub struct Exit {
    /// Process exit status, zero for a normal quit or 128 plus the terminating signal.
    pub code: u8,
}

/// Runtime failure reported only after terminal restoration has been attempted.
#[derive(Debug)]
pub struct Failure {
    message: String,
    code: u8,
}

impl Failure {
    /// Exit status: 1 for ordinary errors, 101 for an unwinding panic.
    pub fn code(&self) -> u8 {
        self.code
    }
}

impl fmt::Display for Failure {
    /// Present the primary failure and any accompanying restoration failure.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// Start the empty editor shell after validating stdin, stdout, and TERM.
pub fn run() -> Result<Exit, Failure> {
    let (_sender, inbox) = status::mailbox();
    run_with_status(inbox)
}

/// Start the shell with a bounded background mailbox supplied by its caller.
///
/// This function owns the single terminal event reader for the duration of the session.
/// Only one interactive session may run in a process at a time. Callers must not print
/// diagnostics while it is active; the binary suppresses the default panic hook so
/// panic diagnostics are printed after restoration as well.
pub fn run_with_status(inbox: StatusInbox) -> Result<Exit, Failure> {
    let mut session = TerminalSession::new(UnixTerminal);
    guarded(&mut session, |session| {
        terminal::validate()?;
        let signals = Signals::register()?;
        session.acquire()?;
        runtime::run(session, &signals, inbox)
    })
}

/// Catch unwinding while the owner remains alive, then combine primary and cleanup outcomes.
fn guarded<D: TerminalDriver>(
    session: &mut TerminalSession<D>,
    operation: impl FnOnce(&mut TerminalSession<D>) -> io::Result<Exit>,
) -> Result<Exit, Failure> {
    let result = panic::catch_unwind(AssertUnwindSafe(|| operation(session)));
    let cleanup = session.restore();
    let result = match result {
        Ok(result) => result.map_err(|error| Failure {
            message: error.to_string(),
            code: 1,
        }),
        Err(payload) => {
            let text = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("unknown panic");
            Err(Failure {
                message: format!("application panicked: {text}"),
                code: 101,
            })
        }
    };
    match (result, cleanup) {
        (Ok(exit), Ok(())) => Ok(exit),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(Failure {
            message: format!("terminal restoration failed: {error}"),
            code: 1,
        }),
        (Err(mut failure), Err(error)) => {
            failure
                .message
                .push_str(&format!("; terminal restoration also failed: {error}"));
            Err(failure)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Mode;

    /// Force restoration failures to verify diagnostics preserve the primary cause and exit code.
    struct FailingCleanup;

    impl TerminalDriver for FailingCleanup {
        /// Setup succeeds; cleanup always fails to exercise compound failures.
        fn set(&mut self, _mode: Mode, enabled: bool) -> io::Result<()> {
            if enabled {
                Ok(())
            } else {
                Err(io::Error::other("cleanup failed"))
            }
        }
    }

    /// Primary I/O failures and panics survive a second error during terminal restoration.
    #[test]
    fn primary_failure_and_panic_are_preserved() {
        for panic in [false, true] {
            let mut session = TerminalSession::new(FailingCleanup);
            let error = guarded(&mut session, |session| {
                session.acquire()?;
                if panic {
                    panic!("primary panic");
                }
                Err(io::Error::other("primary read failure"))
            })
            .err()
            .unwrap();
            assert_eq!(error.code(), if panic { 101 } else { 1 });
            assert!(error.to_string().contains(if panic {
                "primary panic"
            } else {
                "primary read failure"
            }));
            assert!(error.to_string().contains("restoration also failed"));
        }
    }
}
