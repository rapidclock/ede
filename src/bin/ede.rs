//! Command-line boundary: informational flags never acquire terminal resources.

use std::{env, panic, process::ExitCode};

/// Parse the deliberately small stage-one CLI and report errors outside the terminal session.
fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "ede {}\n\nUsage: ede [--help | --version]\n\nTerminal shell (document editing arrives in a later stage).\nF1: Help    Ctrl+Q: Quit",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        [arg] if arg == "--version" || arg == "-V" => {
            println!("ede {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("ede: unexpected arguments; use --help (opening files is not available yet)");
            return ExitCode::from(2);
        }
    }
    // Printing a panic from the default hook while raw/alternate-screen modes are active
    // corrupts the display. run() catches unwinding and returns its diagnostic after cleanup.
    panic::set_hook(Box::new(|_| {}));
    match ede::run() {
        Ok(exit) => ExitCode::from(exit.code),
        Err(error) => {
            eprintln!("ede: {error}");
            ExitCode::from(error.code())
        }
    }
}
