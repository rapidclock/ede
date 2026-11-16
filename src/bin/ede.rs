//! Command-line boundary: informational flags never acquire terminal resources.

#[path = "ede/allocator.rs"]
mod allocator;

use std::{env, panic, path::PathBuf, process::ExitCode};

/// Parse native path arguments and report usage errors before acquiring the terminal.
fn main() -> ExitCode {
    // Retain only enough arguments to recognize flags and reject an overlong list.
    let args: Vec<_> = env::args_os().skip(1).take(ede::TAB_LIMIT + 2).collect();
    match args.as_slice() {
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
                "ede {}\n\nUsage: ede [--] [FILE ...]\n       ede --help | --version\n\nOpen UTF-8 files in tabs, or create an unnamed buffer.\nF1: Help    F2: Actions    Ctrl+Q: Quit",
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
    let separated = args.first().is_some_and(|arg| arg == "--");
    let paths = &args[usize::from(separated)..];
    if paths.len() > ede::TAB_LIMIT
        || (!separated
            && paths
                .iter()
                .any(|arg| arg.as_encoded_bytes().starts_with(b"-")))
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
    match ede::run_cli(
        paths.iter().map(PathBuf::from).collect(),
        env::current_exe().ok(),
    ) {
        Ok(exit) => ExitCode::from(exit.code),
        Err(error) => {
            eprintln!("ede: {error}");
            ExitCode::from(error.code())
        }
    }
}
