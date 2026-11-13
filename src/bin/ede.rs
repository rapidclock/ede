//! Command-line boundary: informational flags never acquire terminal resources.

use std::{env, panic, process::ExitCode};

/// Parse the deliberately small stage-one CLI and report errors outside the terminal session.
fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let path = match args.as_slice() {
        [] => None,
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "ede {}\n\nUsage: ede [--] [FILE]\n       ede --help | --version\n\nOpen one UTF-8 file, or create an unnamed buffer.\nF1: Help    F2: Actions    Ctrl+Q: Quit",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        [arg] if arg == "--version" || arg == "-V" => {
            println!("ede {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        [arg] if !arg.as_encoded_bytes().starts_with(b"-") => Some(std::path::PathBuf::from(arg)),
        [separator] if separator == "--" => None,
        [separator, path] if separator == "--" => Some(std::path::PathBuf::from(path)),
        _ => {
            eprintln!("ede: expected at most one file path; use --help");
            return ExitCode::from(2);
        }
    };
    // Printing a panic from the default hook while raw/alternate-screen modes are active
    // corrupts the display. run() catches unwinding and returns its diagnostic after cleanup.
    panic::set_hook(Box::new(|_| {}));
    match ede::run_with_path(path) {
        Ok(exit) => ExitCode::from(exit.code),
        Err(error) => {
            eprintln!("ede: {error}");
            ExitCode::from(error.code())
        }
    }
}
