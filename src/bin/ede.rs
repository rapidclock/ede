//! Command-line boundary: informational flags never acquire terminal resources.

#[path = "ede/allocator.rs"]
mod allocator;

use std::{env, panic, path::PathBuf, process::ExitCode};

/// Parse native path arguments and report usage errors before acquiring the terminal.
fn main() -> ExitCode {
    // Retain only enough arguments to recognize flags and reject an overlong list.
    let args: Vec<_> = env::args_os().skip(1).take(ede::TAB_LIMIT + 3).collect();
    match args.as_slice() {
        #[cfg(target_os = "macos")]
        [arg] if arg == "--ede-pasteboard-read" || arg == "--ede-pasteboard-write" => {
            return ExitCode::from(ede::macos_worker(arg == "--ede-pasteboard-write"));
        }
        #[cfg(target_os = "linux")]
        [arg] if arg == "--ede-x11-clipboard-read" || arg == "--ede-x11-clipboard-write" => {
            return ExitCode::from(ede::x11_worker(arg == "--ede-x11-clipboard-write"));
        }
        [arg] if arg == "--ede-syntax-worker" => {
            let result = allocator::enable().and_then(|()| {
                ede::syntax_worker(
                    std::io::stdin().lock(),
                    std::io::stdout().lock(),
                    allocator::counters,
                )
            });
            return if result.is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "ede {}\n\nUsage: ede [--safe-config] [--] [FILE ...]\n       producer | ede [--safe-config] [--] [FILE ...]\n       ede [--safe-config] [--] [FILE ...] < INPUT\n       ede [--safe-config] --follow FILE\n       ede --help | --version\n\nOpen UTF-8 files in tabs. Piped stdin needs a foreground tty and becomes\nan editable scratch at valid EOF. --follow requires terminal stdin and\nopens a read-only file follower. Interactive output needs a terminal.\n--safe-config uses bundled configuration for the session.\nF1: Help    F2: Actions    Ctrl+Q: Quit",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        [arg] if arg == "--version" || arg == "-V" => {
            println!("ede {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let safe = args.first().is_some_and(|arg| arg == "--safe-config");
    let args = &args[usize::from(safe)..];
    let separated = args.first().is_some_and(|arg| arg == "--");
    let follow = args.first().is_some_and(|arg| arg == "--follow");
    let paths = &args[usize::from(separated)..];
    if follow && args.len() != 2 {
        eprintln!("ede: --follow requires exactly one file path");
        return ExitCode::from(2);
    }
    if !follow
        && (paths.len() > ede::TAB_LIMIT
            || (!separated
                && paths
                    .iter()
                    .any(|arg| arg.as_encoded_bytes().starts_with(b"-"))))
    {
        eprintln!(
            "ede: expected at most {} file paths; use -- before dash-prefixed paths",
            ede::TAB_LIMIT
        );
        return ExitCode::from(2);
    };
    // Printing a panic from the default hook while raw/alternate-screen modes are active
    // corrupts the display. run() catches unwinding and returns its diagnostic after cleanup.
    panic::set_hook(Box::new(|_| {}));
    let (paths, follow) = if follow {
        (Vec::new(), Some(PathBuf::from(&args[1])))
    } else {
        (paths.iter().map(PathBuf::from).collect(), None)
    };
    match ede::run_launch_with_config(paths, follow, env::current_exe().ok(), safe) {
        Ok(exit) => ExitCode::from(exit.code),
        Err(error) => {
            eprintln!("ede: {error}");
            ExitCode::from(error.code())
        }
    }
}
