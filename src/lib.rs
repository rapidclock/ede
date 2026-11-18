//! Interactive paged text editor. Application state is independent of terminal ownership.

mod app;
mod clipboard;
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use clipboard::macos_worker;
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub use clipboard::x11_worker;
#[doc(hidden)]
pub use syntax::serve as syntax_worker;
mod command;
pub mod document;
mod editor;
mod ingress;
mod input;
mod picker;
mod recovery;
mod runtime;
pub mod search;
mod settings;
mod spool;
pub mod status;
mod stream;
mod syntax;
mod terminal;
mod text;
mod ui;

use std::{
    fmt, io,
    panic::{self, AssertUnwindSafe},
};
use std::{fs::File, io::IsTerminal, os::fd::AsFd, path::PathBuf};

use status::StatusInbox;
use terminal::{Signals, TerminalDriver, TerminalSession, UnixTerminal};

/// Maximum number of tabs and startup file arguments per session.
pub const TAB_LIMIT: usize = 64;

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

impl From<io::Error> for Failure {
    /// Startup/runtime I/O failures use the ordinary error exit status.
    fn from(error: io::Error) -> Self {
        Self {
            message: error.to_string(),
            code: 1,
        }
    }
}

/// Start an unnamed editor document after validating stdin, stdout, and TERM.
pub fn run() -> Result<Exit, Failure> {
    let (_sender, inbox) = status::mailbox();
    run_session(inbox, Vec::new(), None)
}

/// Start an unnamed editing session with a bounded background mailbox supplied by its caller.
///
/// This function owns the single terminal event reader for the duration of the session.
/// Only one interactive session may run in a process at a time. Callers must not print
/// diagnostics while it is active; the binary suppresses the default panic hook so
/// panic diagnostics are printed after restoration as well.
pub fn run_with_status(inbox: StatusInbox) -> Result<Exit, Failure> {
    run_session(inbox, Vec::new(), None)
}
/// Start a single-document editing session with an optional OS-native file path.
pub fn run_with_path(path: Option<PathBuf>) -> Result<Exit, Failure> {
    run_with_paths(path.into_iter().collect())
}
/// Start a session with OS-native paths in argument order.
pub fn run_with_paths(paths: Vec<PathBuf>) -> Result<Exit, Failure> {
    let (_sender, inbox) = status::mailbox();
    run_session(inbox, paths, None)
}

/// Start the ede CLI with an executable that implements the private syntax protocol.
#[doc(hidden)]
pub fn run_cli(paths: Vec<PathBuf>, executable: Option<PathBuf>) -> Result<Exit, Failure> {
    run_launch(paths, None, executable)
}

/// Validate CLI stream admission before terminal mode changes or recovery claims.
#[doc(hidden)]
pub fn run_launch(
    paths: Vec<PathBuf>,
    follow: Option<PathBuf>,
    executable: Option<PathBuf>,
) -> Result<Exit, Failure> {
    run_launch_with_config(paths, follow, executable, false)
}

/// Run the CLI, optionally disabling user configuration for the session.
#[doc(hidden)]
pub fn run_launch_with_config(
    paths: Vec<PathBuf>,
    follow: Option<PathBuf>,
    executable: Option<PathBuf>,
    safe: bool,
) -> Result<Exit, Failure> {
    use nix::sys::stat::{SFlag, fstat};
    let stdin = io::stdin();
    let terminal_input = stdin.is_terminal();
    if paths.len() > TAB_LIMIT - usize::from(!terminal_input)
        || (follow.is_some() && (!terminal_input || !paths.is_empty()))
    {
        return Err(Failure {
            message: if follow.is_some() {
                "--follow requires terminal stdin and exactly one file".into()
            } else {
                format!("Maximum {TAB_LIMIT} startup tabs, including Stdin")
            },
            code: 2,
        });
    }
    terminal::validate_output()?;
    let (terminal, source) = if terminal_input {
        (UnixTerminal::default(), follow.map(stream::Source::Follow))
    } else {
        let kind = SFlag::from_bits_truncate(fstat(&stdin).map_err(io::Error::from)?.st_mode)
            & SFlag::S_IFMT;
        if !matches!(kind, SFlag::S_IFIFO | SFlag::S_IFREG) {
            return Err(Failure::from(io::Error::other(
                "interactive mode requires terminal, pipe, or regular-file stdin",
            )));
        }
        let terminal = UnixTerminal::controlling().map_err(|error| {
            io::Error::other(format!(
                "piped input requires a foreground controlling terminal: {error}"
            ))
        })?;
        let inherited = File::from(stdin.as_fd().try_clone_to_owned()?);
        // Replacing fd 0 removes the extra pipe reader before spawning any helper.
        nix::unistd::dup2_stdin(
            terminal
                .input()?
                .ok_or_else(|| io::Error::other("missing controlling terminal"))?,
        )
        .map_err(io::Error::from)?;
        (terminal, Some(stream::Source::Stdin(inherited)))
    };
    provide_descriptor_headroom();
    let (_sender, inbox) = status::mailbox();
    run_validated(
        inbox,
        stream::Startup { paths, source },
        executable,
        terminal,
        safe,
    )
}

/// Raise the process's soft limit, when permitted, before opening document stores.
/// A full session needs four files per tab plus descriptors for snapshots, I/O and workers.
fn provide_descriptor_headroom() {
    use nix::sys::resource::{Resource::RLIMIT_NOFILE, getrlimit, setrlimit};

    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if let Ok((soft, hard)) = getrlimit(RLIMIT_NOFILE) {
            let target = hard.min(1024);
            if soft < target {
                let _ = setrlimit(RLIMIT_NOFILE, target, hard);
            }
        }
    });
}

/// Shared lifecycle boundary for embedded callers and command-line document startup.
fn run_session(
    inbox: StatusInbox,
    paths: Vec<PathBuf>,
    executable: Option<PathBuf>,
) -> Result<Exit, Failure> {
    if paths.len() > TAB_LIMIT {
        return Err(Failure {
            message: format!("Expected at most {TAB_LIMIT} startup paths"),
            code: 2,
        });
    }
    terminal::validate()?;
    run_validated(
        inbox,
        stream::Startup {
            paths,
            source: None,
        },
        executable,
        UnixTerminal::default(),
        false,
    )
}

fn run_validated(
    inbox: StatusInbox,
    startup: stream::Startup,
    executable: Option<PathBuf>,
    terminal: UnixTerminal,
    safe: bool,
) -> Result<Exit, Failure> {
    let launch_directory = std::env::current_dir()?;
    let recovery = recovery::Store::open(recovery::Options::environment()?)?;
    let desktop_clipboard = if safe {
        None
    } else {
        clipboard::discover(executable.as_deref())
    };
    // Keep signal subscriptions alive through explicit restoration and the Drop retry.
    let signals = Signals::register()?;
    let mut session = TerminalSession::new(terminal);
    let result = guarded(&mut session, |session| {
        session.acquire()?;
        runtime::run(
            session,
            &signals,
            inbox,
            runtime::Launch {
                directory: launch_directory,
                documents: startup,
                recovery,
                syntax_program: executable,
                desktop_clipboard,
                config: settings::Location::discover(safe),
            },
        )
    });
    if session.output_lost() {
        // The client's parser may still be inside the abandoned control string.
        return Ok(result.unwrap_or(Exit { code: 1 }));
    }
    result
}

/// Catch unwinding while the owner remains alive, then combine primary and cleanup outcomes.
fn guarded<D: TerminalDriver>(
    session: &mut TerminalSession<D>,
    operation: impl FnOnce(&mut TerminalSession<D>) -> io::Result<Exit>,
) -> Result<Exit, Failure> {
    let result = panic::catch_unwind(AssertUnwindSafe(|| operation(session)));
    let cleanup = session.restore();
    let result = match result {
        Ok(result) => result.map_err(Failure::from),
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
