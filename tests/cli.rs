//! Process-level CLI checks require neither a terminal nor terminal escape parsing.

use nix::unistd::{pipe, write};
use std::{
    io::Read,
    os::fd::OwnedFd,
    process::{Command, Stdio},
};

/// Run the compiled binary with redirected streams.
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ede"));
    command.stdin(Stdio::null()).env("TERM", "xterm-256color");
    command
}

/// Every diagnostic/informational path must remain plain text when redirected.
fn assert_plain(bytes: &[u8]) {
    assert!(
        !bytes.contains(&0x1b),
        "unexpected escape sequence: {bytes:?}"
    );
}

/// Help/version precede terminal validation, while unrecognized arguments use exit code two.
#[test]
fn informational_flags_and_argument_errors() {
    for flag in ["--help", "-h", "--version", "-V"] {
        let output = command().arg(flag).env_remove("TERM").output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(env!("CARGO_PKG_VERSION")));
        assert!(output.stderr.is_empty());
        assert_plain(&output.stdout);
    }
    for args in [vec!["file.txt"], vec!["--unknown"], vec!["--help", "extra"]] {
        let output = command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_plain(&output.stderr);
    }
}

/// Reject a pipe before consuming any bytes; retain a duplicate reader to prove this.
#[test]
fn non_tty_input_is_not_consumed() {
    let (read, write_end) = pipe().unwrap();
    write(&write_end, b"retained pipe data").unwrap();
    drop(write_end);
    let duplicate: OwnedFd = read.try_clone().unwrap();
    let output = command().stdin(Stdio::from(duplicate)).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_plain(&output.stderr);
    let mut retained = String::new();
    std::fs::File::from(read)
        .read_to_string(&mut retained)
        .unwrap();
    assert_eq!(retained, "retained pipe data");
}

/// TTY stdin alone is insufficient: redirected stdout must receive no escape sequences.
#[test]
fn redirected_stdout_fails_before_terminal_mutation() {
    let pty = nix::pty::openpty(None, None).unwrap();
    let original = nix::sys::termios::tcgetattr(&pty.slave).unwrap();
    let output = command()
        .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_plain(&output.stderr);
    assert_eq!(nix::sys::termios::tcgetattr(&pty.slave).unwrap(), original);
}
