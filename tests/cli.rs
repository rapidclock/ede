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
        if matches!(flag, "--help" | "-h") {
            let help = String::from_utf8(output.stdout).unwrap();
            assert!(help.contains("--follow FILE"));
            assert!(help.contains("producer | ede"));
        }
    }
    for args in [
        vec!["--unknown"],
        vec!["--help", "extra"],
        vec!["file"; 65],
        vec!["--follow"],
        vec!["--follow", "first", "second"],
        vec!["first", "--follow", "second"],
    ] {
        let output = command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_plain(&output.stderr);
    }
}

/// A pipe takes one startup slot and cannot be combined with direct following.
#[test]
fn incompatible_stream_startup_leaves_input_unread() {
    for args in [vec!["file"; 64], vec!["--follow", "file"]] {
        let (read, write_end) = pipe().unwrap();
        write(&write_end, b"retain me").unwrap();
        drop(write_end);
        let output = command()
            .args(args)
            .stdin(Stdio::from(read.try_clone().unwrap()))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_plain(&output.stderr);
        let mut retained = String::new();
        std::fs::File::from(read)
            .read_to_string(&mut retained)
            .unwrap();
        assert_eq!(retained, "retain me");
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

/// A tty for stdout alone does not give the child a controlling terminal.
#[test]
fn stdout_pty_without_controlling_terminal_leaves_pipe_and_modes_intact() {
    use std::os::unix::process::CommandExt;

    let pty = nix::pty::openpty(None, None).unwrap();
    let original = nix::sys::termios::tcgetattr(&pty.slave).unwrap();
    let (reader, writer) = pipe().unwrap();
    write(&writer, b"must stay unread").unwrap();
    drop(writer);
    let mut child = command();
    child
        .stdin(Stdio::from(reader.try_clone().unwrap()))
        .stdout(Stdio::from(pty.slave.try_clone().unwrap()));
    // SAFETY: setsid is async-signal-safe; the pre-exec hook uses no inherited locks.
    unsafe {
        child.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(Into::into));
    }
    let output = child.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("controlling terminal"));
    assert_plain(&output.stderr);
    assert_eq!(nix::sys::termios::tcgetattr(&pty.slave).unwrap(), original);
    let flags = nix::fcntl::OFlag::O_NONBLOCK;
    nix::fcntl::fcntl(&pty.master, nix::fcntl::FcntlArg::F_SETFL(flags)).unwrap();
    assert_eq!(
        nix::unistd::read(&pty.master, &mut [0; 1]),
        Err(nix::errno::Errno::EAGAIN)
    );
    let mut retained = String::new();
    std::fs::File::from(reader)
        .read_to_string(&mut retained)
        .unwrap();
    assert_eq!(retained, "must stay unread");
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

/// Native paths and -- are accepted before terminal validation.
#[test]
fn file_paths_are_accepted_without_consuming_redirected_input() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    for args in [
        vec![OsString::from("file.txt")],
        vec!["file.txt".into(), "another.txt".into()],
        vec!["--".into(), "--help".into()],
        vec!["--".into(), "-first".into(), "-second".into()],
        vec![OsString::from_vec(b"source-\xff".to_vec())],
        vec!["--".into()],
    ] {
        let output = command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("requires terminal"));
        assert_plain(&output.stderr);
    }
}
