//! Unix pseudo-terminal acceptance tests against the actual executable and kernel termios.
//!
//! These exercise terminal protocols and job control, not a graphical terminal emulator.

use std::{
    fs::File,
    io::{self, Read, Write},
    num::NonZeroU16,
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    libc,
    pty::{Winsize, openpty},
    sys::{
        signal::{Signal, kill},
        termios::{self, InputFlags, SetArg, Termios},
    },
    unistd::{Pid, setsid},
};

/// Bounded timeouts turn a broken terminal loop into a useful test failure, not a hanging CI job.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Stage 06 must expose a real non-modal search surface through portable input.
#[test]
fn literal_find_panel_uses_unsaved_text() {
    let mut terminal = PtyChild::editor();
    terminal.expect("Ready");
    terminal.send(b"one two one");
    terminal.expect("one two one");
    terminal.send(b"\x06");
    terminal.expect("Find:");
    terminal.send(b"one\r");
    terminal.expect("2 matches");
    terminal.send(b"\x1b");
    terminal.expect_absent("Find:");
    terminal.send(b"\x11");
    terminal.wait();
}

/// Portable Replace dispatch, confirmation, one atomic batch and ordinary Undo.
#[test]
fn literal_replace_all_confirmation_and_undo() {
    let mut terminal = PtyChild::editor();
    terminal.expect("Ready");
    terminal.send(b"one two one");
    terminal.expect("one two one");
    terminal.send(b"\x08");
    terminal.expect("Replace:");
    terminal.send(b"one\t1\t\t\t\t\t\t\t\r");
    terminal.expect("Replace all — confirm scope");
    terminal.send(b"\t\r");
    terminal.expect("2 occurrences processed");
    terminal.send(b"\x1b");
    terminal.expect_absent("Replace all…");
    terminal.expect("1 two 1");
    terminal.send(b"\x1a");
    terminal.expect("one two one");
    terminal.send(b"\x11");
    assert!(terminal.wait().success());
    terminal.assert_restored();
}

/// Test terminal, retained slave attributes, process, and a bounded diagnostic transcript.
struct PtyChild {
    _state_root: Option<tempfile::TempDir>,
    child: Child,
    master: File,
    slave: File,
    original: Termios,
    output: Vec<u8>,
    parser: vt100::Parser,
    keep_alive: bool,
    restored: Option<Termios>,
}

impl PtyChild {
    /// Spawn with a controlling terminal. Unsafe operations run only between fork and exec.
    fn spawn(mut command: Command) -> Self {
        let state_root = if command
            .get_envs()
            .any(|(name, value)| name == "EDE_STATE_DIR" && value.is_some())
        {
            None
        } else {
            let directory = tempfile::tempdir().unwrap();
            command.env("EDE_STATE_DIR", directory.path().join("state"));
            Some(directory)
        };
        let pty = openpty(
            Some(&Winsize {
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        let slave = File::from(pty.slave);
        let mut original = termios::tcgetattr(&slave).unwrap();
        original.input_flags.insert(InputFlags::IXON);
        termios::tcsetattr(&slave, SetArg::TCSANOW, &original).unwrap();
        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .env("TERM", "xterm-256color")
            .env("PS1", "EDE_TEST_PROMPT> ");
        // SAFETY: setsid and ioctl are async-signal-safe; fd 0 is the already-duplicated
        // slave PTY. This closure performs no allocation, locking, or Rust-owned mutation.
        unsafe {
            command.pre_exec(|| {
                setsid().map_err(io::Error::from)?;
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        fcntl(&pty.master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();
        Self {
            _state_root: state_root,
            child,
            master: File::from(pty.master),
            slave,
            original,
            output: Vec::new(),
            parser: vt100::Parser::new(
                NonZeroU16::new(24).unwrap(),
                NonZeroU16::new(80).unwrap(),
                0,
            ),
            keep_alive: false,
            restored: None,
        }
    }

    /// Keep the parent shell alive until attributes are captured; macOS revokes a PTY on hangup.
    fn editor() -> Self {
        Self::editor_with_term(Some("xterm-256color"))
    }

    /// Read all currently available output without blocking.
    fn drain(&mut self) {
        let mut bytes = [0; 8192];
        loop {
            match self.master.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    self.parser.process(&bytes[..count]);
                    self.output.extend_from_slice(&bytes[..count]);
                    assert!(self.output.len() < 1024 * 1024, "runaway terminal output");
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("PTY read: {error}"),
            }
        }
    }

    /// Wait for actual displayed text; cursor positioning can split words in the raw stream.
    fn expect(&mut self, text: &str) {
        self.expect_with_timeout(text, TIMEOUT);
    }
    /// Larger fixture preparation and long-line seeks have separately measured deadlines.
    fn expect_with_timeout(&mut self, text: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain();
            if self.parser.screen().contents().contains(text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing {text:?}: {}",
                String::from_utf8_lossy(&self.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    /// Wait until an overlay is actually dismissed before sending a dependent key.
    fn expect_absent(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            let screen = self.parser.screen().contents();
            if !screen.contains(text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "still showing {text:?}: {screen}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Observe document rows rather than a matching filename, help label or status message.
    fn expect_body(&mut self, text: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain();
            let height = self.parser.screen().size().0.get();
            if self
                .parser
                .screen()
                .rows(0, 80)
                .skip(1)
                .take(height.saturating_sub(2) as usize)
                .any(|row| row.contains(text))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing document text {text:?}: {}",
                self.parser.screen().contents()
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    /// Wait for plain shell output or a synchronization marker produced after application exit.
    fn expect_raw(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            if self
                .output
                .windows(text.len())
                .any(|window| window == text.as_bytes())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing raw {text:?}: {}",
                String::from_utf8_lossy(&self.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Assert feedback appears in the status row rather than matching the empty document label.
    fn expect_status(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            if self
                .parser
                .screen()
                .rows(0, 80)
                .last()
                .unwrap()
                .contains(text)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing status {text:?}: {}",
                self.parser.screen().contents()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Separate subsequent assertions from text drawn by earlier frames.
    fn reset_output(&mut self) {
        self.drain();
        self.output.clear();
    }

    /// Send actual bytes a conventional terminal would transmit.
    fn send(&mut self, bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut remaining = bytes;
        while !remaining.is_empty() {
            match self.master.write(&remaining[..remaining.len().min(8192)]) {
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.drain();
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => panic!("PTY write: {error}"),
            }
            assert!(Instant::now() < deadline, "terminal input stalled");
        }
    }

    /// The foreground launcher reports its PID before exec; no cross-session ioctl is needed.
    fn editor_pid(&self) -> Pid {
        let text = String::from_utf8_lossy(&self.output);
        let start = text.rfind("EDE_PID:").expect("launcher PID marker") + "EDE_PID:".len();
        let digits: String = text[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        Pid::from_raw(digits.parse().unwrap())
    }
    /// Optional kernel-reported lifetime RSS peak, distinct from live ps samples.
    fn report_peak_rss(&self) {
        if std::env::var_os("EDE_BENCH_PEAK_RSS").is_none() {
            return;
        }
        let output = String::from_utf8_lossy(&self.output);
        let line = output
            .lines()
            .find(|line| {
                line.contains("maximum resident set size")
                    || line.contains("Maximum resident set size (kbytes)")
            })
            .expect("time resource report");
        let value = if cfg!(target_os = "macos") {
            line.split_whitespace()
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                / 1024
        } else {
            line.split(':')
                .next_back()
                .unwrap()
                .trim()
                .parse::<u64>()
                .unwrap()
        };
        eprintln!("kernel_lifetime_peak_editor_RSS_KiB={value}");
    }

    /// Change kernel dimensions and notify the foreground application.
    fn resize(&mut self, width: u16, height: u16) {
        self.parser.set_size(
            NonZeroU16::new(height).unwrap(),
            NonZeroU16::new(width).unwrap(),
        );
        let size = Winsize {
            ws_row: height,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: master is a valid PTY fd and size points to a valid winsize value.
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) },
            0
        );
        // TIOCSWINSZ itself delivers SIGWINCH to the foreground process group.
    }

    /// Wait without risking a stuck CI process, then inspect final restoration.
    fn wait(&mut self) -> ExitStatus {
        if self.keep_alive {
            self.expect_raw("EDE_EXIT:");
            self.restored = Some(termios::tcgetattr(&self.slave).unwrap());
            self.send(b"\n");
            self.keep_alive = false;
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            if let Some(status) = self.child.try_wait().unwrap() {
                self.drain();
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "process did not exit: {}",
                String::from_utf8_lossy(&self.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Compare exact captured kernel attributes and output-mode cleanup requests.
    fn assert_restored(&mut self) {
        assert!(
            !self.output.windows(4).any(|window| window == b"\x1b[6n"),
            "application must not query cursor position"
        );
        let restored = self
            .restored
            .clone()
            .unwrap_or_else(|| termios::tcgetattr(&self.slave).unwrap());
        assert_eq!(restored, self.original);
        self.drain();
        for sequence in [
            "\x1b[?1049l",
            "\x1b[?25h",
            "\x1b[?1000l",
            "\x1b[?2004l",
            "\x1b[?1004l",
        ] {
            assert!(
                self.output
                    .windows(sequence.len())
                    .any(|window| window == sequence.as_bytes()),
                "missing restoration sequence {sequence:?}"
            );
        }
    }
}

impl Drop for PtyChild {
    /// Kill/reap on assertion failure so tests never leave an orphaned editor or shell.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ctrl+S cannot freeze output, Ctrl+C is copy, and pasted control bytes never become commands.
#[test]
fn real_shortcuts_paste_and_cleanup() {
    let mut process = PtyChild::editor();
    process.expect("Ready");
    assert!(
        !termios::tcgetattr(&process.slave)
            .unwrap()
            .input_flags
            .contains(InputFlags::IXON)
    );
    process.send(b"\x13");
    process.expect("Save As (new path)");
    process.send(b"\x1b");
    thread::sleep(Duration::from_millis(60));
    process.send(b"\x03");
    process.expect_status("No selection");
    process.send(b"\x1b[200~\x11\x13secret\x1b[201~");
    process.expect("secret");
    process.expect("<U+0011>");
    assert!(process.child.try_wait().unwrap().is_none());
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Help is modal and mouse transitions use the rendered target rectangles after resize.
#[test]
fn real_mouse_overlay_and_resize() {
    let mut process = PtyChild::editor();
    process.expect("Ready");
    process.reset_output();
    process.send(b"\x1bOP");
    process.expect("Cmd aliases require");
    // Header Quit is underneath the overlay and must not be activated.
    process.send(b"\x1b[<0;74;1M\x1b[<0;74;1mtext");
    process.expect("Cmd aliases require");
    assert!(process.child.try_wait().unwrap().is_none());
    process.reset_output();
    process.resize(20, 4);
    process.expect("Resize");
    process.reset_output();
    process.resize(80, 24);
    process.expect("Cmd aliases require");
    process.send(b"\x1b");
    // Wait for the overlay to disappear before sending another escape-prefixed sequence.
    let deadline = Instant::now() + TIMEOUT;
    while process
        .parser
        .screen()
        .contents()
        .contains("Cmd aliases require")
    {
        process.drain();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    process.send(b"\x1b[<0;74;1M\x1b[<0;74;1m");
    assert!(process.wait().success());
    process.assert_restored();
}

/// External termination remains distinct from Ctrl+C, restores all modes, and reports its signal.
#[test]
fn external_signals_restore_terminal() {
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        let mut process = PtyChild::editor();
        process.expect("Ready");
        kill(process.editor_pid(), signal).unwrap();
        assert_eq!(process.wait().code(), Some(128 + signal as i32));
        process.assert_restored();
    }
}

/// Startup rejects unusable TERM before terminal mutation even with actual TTY descriptors.
#[test]
fn unusable_term_is_a_plain_error() {
    for term in [None, Some("dumb"), Some("")] {
        let mut process = PtyChild::editor_with_term(term);
        assert_eq!(process.wait().code(), Some(1));
        assert!(!process.output.contains(&0x1b));
        assert_eq!(process.restored.as_ref().unwrap(), &process.original);
    }
}

impl PtyChild {
    /// Override TERM after the harness sets up TTY handles, testing the actual child environment.
    fn editor_with_term(term: Option<&str>) -> Self {
        Self::editor_with_path(term, None)
    }
    /// Supply an optional OS-native path through positional shell arguments, without interpolation.
    fn editor_with_path(term: Option<&str>, path: Option<&std::path::Path>) -> Self {
        Self::editor_with_args(term, path)
    }

    /// Supply the original native arguments to the executable under job control.
    fn editor_with_args(
        term: Option<&str>,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
    ) -> Self {
        Self::editor_with_state(term, args, None)
    }

    /// Share only the selected recovery profile across otherwise independent processes.
    fn editor_with_state(
        term: Option<&str>,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
        state: Option<&std::path::Path>,
    ) -> Self {
        // The interactive parent supplies job control and stays alive after ede exits,
        // allowing exact post-exit termios capture before Darwin revokes the slave PTY.
        let mut command = Command::new("/bin/sh");
        let script = "# The nested shell expands its own PID and positional parameters.\n# shellcheck disable=SC2016\nif [ \"$1\" = unset ]; then unset TERM; else TERM=$2; export TERM; fi; shift 2; /bin/sh -c 'printf \"EDE_PID:%s\\n\" \"$$\"; program=$1; shift; exec \"$program\" \"$@\"' ede-child \"$@\"; result=$?; printf '\\nEDE_EXIT:%s\\n' \"$result\"; read -r _completion; exit \"$result\"";
        let script = if std::env::var_os("EDE_BENCH_PEAK_RSS").is_some() {
            script.replacen(
                "; /bin/sh -c",
                if cfg!(target_os = "macos") {
                    "; /usr/bin/time -l /bin/sh -c"
                } else {
                    "; /usr/bin/time -v /bin/sh -c"
                },
                1,
            )
        } else {
            script.into()
        };
        command.args([
            "-i",
            "-c",
            script.as_str(),
            "ede-term-test",
            if term.is_none() { "unset" } else { "set" },
            term.unwrap_or(""),
            env!("CARGO_BIN_EXE_ede"),
        ]);
        command.args(args);
        if let Some(state) = state {
            command.env("EDE_STATE_DIR", state);
        }
        let mut child = Self::spawn(command);
        child.keep_alive = true;
        child
    }
}

/// R01: Quit retains scratch text without asking for a filename or discard decision.
#[test]
fn scratch_quit_and_relaunch_retains_text_without_save_prompts() {
    let state = tempfile::tempdir().unwrap();
    let profile = state.path().join("state");
    let mut first = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    first.expect("Ready");
    first.send("retained scratch 界e\u{301}".as_bytes());
    first.expect_body("retained scratch", TIMEOUT);
    first.send(b"\x11");
    assert!(first.wait().success());
    first.assert_restored();
    let mut second = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    second.expect_body("retained scratch 界e\u{301}", TIMEOUT);
    second.send(b"\x1a");
    second.expect_body("retained scratch 界e\u{301}", TIMEOUT);
    second.send(b"\x11");
    assert!(second.wait().success());
    second.assert_restored();
}

/// Replacement revisions participate in normal-exit scratch retention.
#[test]
fn replaced_scratch_is_restored_after_normal_quit() {
    let state = tempfile::tempdir().unwrap();
    let profile = state.path().join("state");
    let mut first = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    first.expect("Ready");
    first.send(b"one two one\x08one\t1\t\t\t\t\t\t\t\r");
    first.expect("Replace all — confirm scope");
    first.send(b"\t\r");
    first.expect("2 occurrences processed");
    first.send(b"\x1b");
    first.expect_absent("Replace all…");
    first.send(b"\x11");
    assert!(first.wait().success());
    first.assert_restored();
    let mut second = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    second.expect_body("1 two 1", TIMEOUT);
    second.send(b"\x11");
    assert!(second.wait().success());
    second.assert_restored();
}

/// F01: An external named-file save refreshes the displayed text without terminal input.
#[test]
fn clean_named_file_refreshes_without_user_input() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("refresh.txt");
    std::fs::write(&path, "original disk contents").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_body("original disk contents", TIMEOUT);
    process.expect("Ready");
    std::fs::write(&path, "updated by another process").unwrap();
    process.expect_body("updated by another process", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A valid Linux XDG state root does not depend on HOME being set.
#[test]
#[cfg(target_os = "linux")]
fn xdg_state_root_works_without_home() {
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        "unset HOME EDE_STATE_DIR; exec \"$1\"",
        "ede-xdg",
        env!("CARGO_BIN_EXE_ede"),
    ]);
    command.env("XDG_STATE_HOME", directory.path());
    let mut process = PtyChild::spawn(command);
    process.expect_status("Ready");
    process.send(b"\x11");
    assert!(process.wait().success());
    assert!(directory.path().join("ede/catalog").exists());
}

/// F01/F04: symlink retargets, rename saves and transient invalid sources preserve last-good text.
#[test]
fn refresh_recovers_after_missing_invalid_and_retargeted_sources() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("watched");
    std::fs::write(&path, "last good text").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_body("last good text", TIMEOUT);
    process.expect_status("Ready");
    std::fs::remove_file(&path).unwrap();
    process.expect("File refresh unavailable");
    process.expect_body("last good text", TIMEOUT);
    std::fs::write(&path, b"\xffinvalid").unwrap();
    process.expect("File refresh failed");
    process.expect_body("last good text", TIMEOUT);
    let target = directory.path().join("target");
    std::fs::write(&target, "valid symlink target").unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink(&target, &path).unwrap();
    process.expect_body("valid symlink target", TIMEOUT);
    let replacement = directory.path().join("replacement");
    std::fs::write(&replacement, "rename saved").unwrap();
    std::fs::rename(&replacement, &target).unwrap();
    process.expect_body("rename saved", TIMEOUT);
    process.send(b"local ");
    process.expect_body("local rename saved", TIMEOUT);
    std::fs::write(&target, "external while dirty").unwrap();
    process.expect("local edits preserved");
    process.expect_body("local rename saved", TIMEOUT);
    process.send(b"\x13");
    process.expect("work retained");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "external while dirty"
    );
    process.send(b"\x1b");
    process.expect_absent("work retained");
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
}

/// R04/R05/I01: retired profiles combine, while excess scratches stay operable through Recovery.
#[test]
fn independent_instances_restore_overflow_and_open_after_freeing_a_slot() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("state");
    let launch = || {
        PtyChild::editor_with_state(
            Some("xterm-256color"),
            std::iter::empty::<&str>(),
            Some(&profile),
        )
    };
    let mut a = launch();
    a.expect_status("Ready");
    let mut b = launch();
    b.expect_status("Ready");
    a.send(&[0x0e; 32]);
    a.expect("Untitled 33");
    b.send(&[0x0e; 32]);
    b.expect("Untitled 33");
    a.send(b"\x11");
    assert!(a.wait().success());
    b.send(b"\x11");
    assert!(b.wait().success());
    let mut c = launch();
    c.expect_with_timeout("Untitled 33", Duration::from_secs(15));
    c.expect_status("Ready");
    let recovery_menu = [
        b"\x1bOQ".as_slice(),
        b"\x1b[B".repeat(18).as_slice(),
        b"\r".as_slice(),
    ]
    .concat();
    c.send(&recovery_menu);
    c.expect("Recovery — retained scratches");
    c.expect("Scratch 65");
    c.expect("Scratch 66");
    c.send(b"\r");
    c.expect("64 open tabs");
    c.send(b"\x1b");
    c.expect_absent("Details");
    c.send(b"\x17");
    c.expect("Unsaved changes");
    c.send(b"\t\t\r");
    c.expect("> ~Untitled 34");
    c.send(&recovery_menu);
    c.expect("Scratch 65");
    c.send(b"\r");
    c.expect("> ~Untitled 65");
    c.send(b"\x11");
    assert!(c.wait().success());
    c.assert_restored();
}

/// Startup order, keyboard tab switching, and tab-local edit history reach the executable.
#[test]
fn startup_tabs_keep_independent_contents_and_history() {
    let directory = tempfile::tempdir().unwrap();
    let a = directory.path().join("alpha.txt");
    let b = directory.path().join("beta.txt");
    std::fs::write(&a, "alpha body").unwrap();
    std::fs::write(&b, "beta body").unwrap();
    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), [&a, &b, &a]);
    process.expect_body("alpha body", TIMEOUT);
    process.expect_status("Ready");
    process.send("界\x1b[19~".as_bytes()); // Next tab (F8).
    process.expect_body("beta body", TIMEOUT);
    process.send(b"B\x1b[18~"); // Previous tab (F7).
    process.expect_body("界alpha body", TIMEOUT);
    process.send(b"\x1a\x1b[19~");
    process.expect_body("Bbeta body", TIMEOUT);
    process.send(b"\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha body");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "Bbeta body");
    process.send(b"\x17");
    process.expect_body("alpha body", TIMEOUT);
    process.send(b"\x17");
    process.expect("No open tabs");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Cancelling quit on a later tab must retain every earlier unsaved buffer.
#[test]
fn cancel_quit_preserves_tabs_after_earlier_discard_choice() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.txt");
    let second = directory.path().join("second.txt");
    std::fs::write(&first, "").unwrap();
    std::fs::write(&second, "").unwrap();
    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), [&first, &second]);
    process.expect_status("Ready");
    process.send(b"first-unsaved");
    process.expect_body("first-unsaved", TIMEOUT);
    process.send(b"\x1b[19~");
    process.expect("> second.txt");
    process.send(b"second-unsaved");
    process.expect_body("second-unsaved", TIMEOUT);
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.expect("first.txt");
    process.send(b"\t\t\r");
    process.expect("second.txt");
    process.send(b"\r"); // Default Cancel.
    process.expect_absent("Unsaved changes");
    process.expect_body("second-unsaved", TIMEOUT);
    process.send(b"\x1b[18~");
    process.expect_body("first-unsaved", TIMEOUT);
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.expect("first.txt");
    process.send(b"\t\t\r");
    process.expect("second.txt");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
}

/// The picker navigates native entries and accepts a pasted relative path literally.
#[test]
fn picker_browses_directories_and_opens_literal_pasted_path() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.txt");
    let child = directory.path().join("child");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(&original, "original body").unwrap();
    std::fs::write(child.join(".file with spaces"), "picked body").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&original));
    process.expect_status("Ready");
    process.send(b"\x0f");
    process.expect("Open file");
    process.expect("child/");
    process.send(b"\r");
    process.expect(".file with spaces");
    process.send(b"\t\x1b[200~.file with spaces\x1b[201~\r");
    process.expect_body("picked body", TIMEOUT);
    process.send(b"\x1b[18~");
    process.expect_body("original body", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Earlier accepted input is applied before a close prompt, even at a fresh tab.
#[test]
fn close_unnamed_save_as_cancel_collision_and_success() {
    let directory = tempfile::tempdir().unwrap();
    let other = directory.path().join("existing.txt");
    let saved = directory.path().join("saved.txt");
    std::fs::write(&other, "untouched").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&other));
    process.expect_status("Ready");
    process.send(b"\x0e");
    process.expect("Untitled 1");
    process.send(b"local edits\x17");
    process.expect("Close — Untitled 1");
    process.send(b"\t\r");
    process.expect("Save As (new path)");
    process.send(b"\x1b");
    process.expect_body("local edits", TIMEOUT);
    process.expect_absent("Save As (new path)");
    process.send(b"\x17");
    process.expect("Close — Untitled 1");
    process.send(b"\t\r");
    process.expect("Save As (new path)");
    process.send(other.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect("destination is busy");
    process.send(b"\x1b");
    process.expect_body("local edits", TIMEOUT);
    process.expect_absent("Save As (new path)");
    process.expect_absent("Details");
    process.send(b"\x17");
    process.expect("Close — Untitled 1");
    process.send(b"\t\r");
    process.expect("Save As (new path)");
    process.send(saved.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_body("untouched", TIMEOUT);
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), "local edits");
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "untouched");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Report every startup error, retain successful neighbors, and fall back if all fail.
#[test]
fn startup_reports_each_failure_and_falls_back() {
    let directory = tempfile::tempdir().unwrap();
    let a = directory.path().join("missing-A");
    let b = directory.path().join("missing-B");
    let good = directory.path().join("-dash");
    std::fs::write(&good, "dash body").unwrap();
    let mut process = PtyChild::editor_with_args(
        Some("xterm-256color"),
        [
            std::ffi::OsStr::new("--"),
            a.as_os_str(),
            good.as_os_str(),
            b.as_os_str(),
        ],
    );
    process.expect("Open missing-A:");
    process.send(b"\r");
    process.expect("Open missing-B:");
    process.expect("Details");
    process.send(b"\r");
    process.expect_absent("Details");
    process.expect_body("dash body", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();

    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), [&a, &b]);
    process.expect("Open missing-A:");
    process.send(b"\r");
    process.expect("Open missing-B:");
    process.expect("Details");
    process.send(b"\r");
    process.expect_absent("Details");
    process.expect_status("Ready");
    process.expect("Untitled 1");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// The overflow list closes an inactive tab by mouse and rejects a stale strip release.
#[test]
fn mouse_tabs_overflow_and_stale_release() {
    let directory = tempfile::tempdir().unwrap();
    let names: Vec<_> = (0..7)
        .map(|n| directory.path().join(format!("tab-{n}")))
        .collect();
    for (n, path) in names.iter().enumerate() {
        std::fs::write(path, format!("body-{n}")).unwrap();
    }
    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), &names);
    process.expect_body("body-0", TIMEOUT);
    process.expect_status("Ready");
    process.send(b"\x1b[<0;14;1M\x1b[19~"); // Press the next tab, then switch via F8.
    process.expect_body("body-1", TIMEOUT);
    process.send(b"\x1b[<0;14;1m\x1b[<0;52;1M\x1b[<0;52;1m");
    process.expect("Tabs — Enter to switch");
    process.expect("tab-6");
    process.send(b"\x1b[<0;7;9M\x1b[<0;7;9m"); // Highlight tab 5.
    process.expect("[Switch]");
    process.send(b"\x1b[<0;27;21M\x1b[<0;27;21m");
    process.expect_body("body-5", TIMEOUT);
    process.send(b"\x1b[18~\x1b[18~\x1b[18~\x1b[18~");
    process.expect_body("body-1", TIMEOUT);
    process.send(b"\x1b[<0;52;1M\x1b[<0;52;1m");
    process.expect("Tabs — Enter to switch");
    process.send(b"\x1b[<0;7;10M\x1b[<0;7;10m"); // Highlight tab 6.
    process.send(b"\x1b[<0;7;21M\x1b[<0;7;21m"); // Close the highlighted inactive tab.
    process.expect_body("body-1", TIMEOUT);
    process.resize(40, 8);
    process.expect("Tabs");
    process.send(b"\x1b[18~\x1b[18~");
    process.expect_body("body-5", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A real interactive shell owns job control: suspend returns cooked mode and fg resumes safely.
#[test]
fn suspend_and_resume_under_job_control() {
    let mut shell = Command::new("/bin/sh");
    shell.arg("-i");
    let mut process = PtyChild::spawn(shell);
    process.expect_raw("EDE_TEST_PROMPT>");
    // Shells can select their own cooked settings; capture exactly what the application inherits.
    process.original = termios::tcgetattr(&process.slave).unwrap();
    process.reset_output();
    process.send(
        format!(
            "/bin/sh -c 'printf \"EDE_PID:%s\\n\" \"$$\"; exec \"$1\"' ede-child '{}'\n",
            env!("CARGO_BIN_EXE_ede")
        )
        .as_bytes(),
    );
    process.expect("Ready");
    let foreground = process.editor_pid();
    process.reset_output();
    process.send(b"\x1bOP");
    process.expect("Cmd aliases require");
    process.send(b"\t\r");
    process.expect_raw("EDE_TEST_PROMPT>");
    process.assert_restored();
    process.resize(60, 16);
    process.reset_output();
    process.send(b"fg\n");
    process.expect("Cmd aliases require");
    assert!(kill(foreground, None).is_ok());
    process.reset_output();
    // External SIGTSTP follows the same cleanup route as the help action.
    kill(foreground, Signal::SIGTSTP).unwrap();
    process.expect_raw("EDE_TEST_PROMPT>");
    process.assert_restored();
    process.reset_output();
    process.send(b"fg\n");
    process.expect("Cmd aliases require");
    process.reset_output();
    process.send(b"\x11");
    process.expect_raw("EDE_TEST_PROMPT>");
    process.assert_restored();
    process.send(b"exit\n");
    assert!(process.wait().success());
}

/// File edits, Unicode, guarded quit, undo and acknowledged save points work through actual input.
#[test]
fn open_edit_save_and_dirty_quit_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file with spaces.txt");
    std::fs::write(&path, b"start\r\nsecond").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("Ready");
    process.expect("start");
    process.send("\x1b[F界e\u{301}".as_bytes());
    process.expect("start界e\u{301}");
    process.send(b"\x13");
    let deadline = Instant::now() + TIMEOUT;
    while std::fs::read(&path).unwrap() != "start界e\u{301}\r\nsecond".as_bytes() {
        process.drain();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    process.expect_status("Saved");
    process.send(b"!\x11");
    process.expect("Unsaved changes");
    process.send(b"\r");
    thread::sleep(Duration::from_millis(30));
    assert!(process.child.try_wait().unwrap().is_none());
    process.send(b"\x1a\x11");
    assert!(process.wait().success());
    process.assert_restored();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        "start界e\u{301}\r\nsecond".as_bytes()
    );
}

/// The terminal parser and document path accept a multi-megabyte paste with one undo unit.
#[test]
fn external_large_paste_save_undo_and_redo() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pasted.txt");
    let mut process = PtyChild::editor();
    process.expect("Ready");
    let mut payload = "line界\r\n".repeat(262_144);
    payload.push_str("END-PAYLOAD");
    assert!(payload.len() > 1 << 20);
    let started = Instant::now();
    process.send(b"\x1b[200~");
    process.send(payload.as_bytes());
    process.send(b"\x1b[201~");
    process.expect("END-PAYLOAD");
    process.send(b"\x13");
    process.expect("Save As (new path)");
    process.send(path.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(&path).unwrap(), payload.as_bytes());
    eprintln!(
        "external paste: {} bytes, receive/render/save {:?}",
        payload.len(),
        started.elapsed()
    );
    process.send(b"\x1a\x11");
    process.expect("Unsaved changes");
    process.send(b"\r");
    thread::sleep(Duration::from_millis(30));
    process.send(b"\x19\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A prompt at tiny geometry cannot accept a hidden Discard/Save button.
#[test]
fn dirty_quit_at_tiny_size_requires_visible_confirmation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("guarded.txt");
    std::fs::write(&path, "").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("Ready");
    process.send(b"work");
    process.expect_body("work", TIMEOUT);
    process.resize(20, 4);
    process.expect("Resize");
    process.send(b"\x11");
    thread::sleep(Duration::from_millis(30));
    process.send(b"\t\t\r\x1b");
    thread::sleep(Duration::from_millis(30));
    assert!(process.child.try_wait().unwrap().is_none());
    process.resize(80, 24);
    process.expect("Unsaved changes");
    process.send(b"\x1b");
    thread::sleep(Duration::from_millis(60));
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Synthetic protocol delivery exercises Cmd/Super and exact Shift, then restores the pushed mode.
#[test]
fn enhanced_cmd_delivery_and_restoration() {
    let mut process = PtyChild::editor();
    process.expect_raw("\x1b[?u");
    process.send(b"\x1b[?1u");
    process.expect_raw("\x1b[>1u");
    process.expect("Ready");
    process.send(b"\x1b[115;10u");
    process.expect("Save As (new path)");
    process.send(b"\x1b");
    thread::sleep(Duration::from_millis(60));
    process.send(b"\x1b[113;9u");
    assert!(process.wait().success());
    process.assert_restored();
    assert!(process.output.windows(5).any(|bytes| bytes == b"\x1b[<1u"));
}

/// I02: checkpoint/restore a large scratch while a GiB refresh keeps another tab usable.
#[test]
#[ignore = "large recovery/refresh benchmark: EDE_BENCH_MIB=1024 EDE_PASTE_MIB=64"]
fn large_recovery_and_refresh_benchmark() {
    use std::os::unix::fs::{FileExt, MetadataExt};
    let mib: u64 = std::env::var("EDE_BENCH_MIB")
        .unwrap_or_else(|_| "1024".into())
        .parse()
        .unwrap();
    let scratch_mib: u64 = std::env::var("EDE_PASTE_MIB")
        .unwrap_or_else(|_| "64".into())
        .parse()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("state");
    let path = directory.path().join("large");
    let replacement = directory.path().join("replacement");
    let companion = directory.path().join("companion");
    let mut chunk = vec![b'x'; 1 << 20];
    for index in (63..chunk.len()).step_by(64) {
        chunk[index] = b'\n';
    }
    for (fixture, marker) in [
        (&path, b"ORIGINAL".as_slice()),
        (&replacement, b"REFRESHED".as_slice()),
    ] {
        let mut file = File::create(fixture).unwrap();
        for index in 0..mib {
            file.write_all(&chunk).unwrap();
            if (index + 1).is_multiple_of(8) {
                file.sync_data().unwrap();
            }
        }
        file.write_all_at(marker, 0).unwrap();
        file.sync_all().unwrap();
    }
    eprintln!(
        "recovery_refresh fixtures_ready named_bytes={} replacement_bytes={} scratch_MiB={scratch_mib}",
        mib * (1 << 20),
        mib * (1 << 20)
    );
    std::fs::write(&companion, "Companion").unwrap();
    let started = Instant::now();
    let mut process =
        PtyChild::editor_with_state(Some("xterm-256color"), [&path, &companion], Some(&profile));
    process.expect_body("ORIGINAL", Duration::from_secs(120));
    let first_text = started.elapsed();
    process.expect_with_timeout("Ready", Duration::from_secs(120));
    let ready = started.elapsed();
    let pid = process.editor_pid();
    let rss = || {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.as_raw().to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let mut peak_rss = rss();
    process.send(b"\x0e");
    process.expect("Untitled");
    process.expect_status("Ready");
    let paste_started = Instant::now();
    process.send(b"\x1b[200~");
    for index in 0..scratch_mib * 16 {
        process.send(&chunk[..64 << 10]);
        if index.is_multiple_of(128) {
            peak_rss = peak_rss.max(rss());
        }
    }
    process.send(b"SCRATCH_END\x1b[201~");
    process.expect_body("SCRATCH_END", Duration::from_secs(120));
    let paste_ready = paste_started.elapsed();
    process.send(b"\x1b[18~");
    process.expect_body("Companion", TIMEOUT);
    std::fs::rename(&replacement, &path).unwrap();
    let refresh_started = Instant::now();
    let mut latency = Vec::new();
    for count in 1..=50 {
        let event = Instant::now();
        process.send(b"Z");
        process.expect_body(&"Z".repeat(count), TIMEOUT);
        latency.push(event.elapsed().as_secs_f64() * 1000.0);
        peak_rss = peak_rss.max(rss());
        thread::sleep(Duration::from_millis(100));
    }
    process.send(b"\x1b[18~");
    process.expect_body("REFRESHED", Duration::from_secs(120));
    let refresh_visible = refresh_started.elapsed();
    latency.sort_by(f64::total_cmp);
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    let quit = Instant::now();
    assert!(process.wait().success());
    let flush = quit.elapsed();
    process.assert_restored();
    let mut object_bytes = 0;
    let mut object_allocated = 0;
    for entry in std::fs::read_dir(profile.join("objects")).unwrap() {
        let metadata = entry.unwrap().metadata().unwrap();
        object_bytes += metadata.len();
        object_allocated += metadata.blocks() * 512;
    }
    let restored_at = Instant::now();
    let mut restored = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    restored.expect_body("SCRATCH_END", Duration::from_secs(120));
    let restore = restored_at.elapsed();
    restored.send(b"\x11");
    assert!(restored.wait().success());
    restored.assert_restored();
    eprintln!(
        "recovery_refresh named_MiB={mib} scratch_MiB={scratch_mib} first_text_ms={:.3} ready_ms={:.3} paste_ready_ms={:.3} refresh_observed_ms={:.3} quit_flush_ms={:.3} restore_ms={:.3} input_p50_ms={:.3} input_p95_ms={:.3} input_max_ms={:.3} sampled_RSS_KiB={peak_rss} recovery_logical_bytes={object_bytes} recovery_allocated_bytes={object_allocated}",
        first_text.as_secs_f64() * 1000.0,
        ready.as_secs_f64() * 1000.0,
        paste_ready.as_secs_f64() * 1000.0,
        refresh_visible.as_secs_f64() * 1000.0,
        flush.as_secs_f64() * 1000.0,
        restore.as_secs_f64() * 1000.0,
        latency[25],
        latency[47],
        latency[49]
    );
    process.report_peak_rss();
}

/// Real allocated-file terminal benchmark; opt in so ordinary tests remain small and repeatable.
#[test]
#[ignore = "large-file benchmark: run explicitly with EDE_BENCH_MIB and EDE_BENCH_CORPUS"]
fn large_file_interactive_benchmark() {
    use std::os::unix::fs::FileExt;
    let mib: u64 = std::env::var("EDE_BENCH_MIB")
        .unwrap_or_else(|_| "1024".into())
        .parse()
        .unwrap();
    let corpus = std::env::var("EDE_BENCH_CORPUS").unwrap_or_else(|_| "ordinary".into());
    let pattern: &[u8] = match corpus.as_str() {
        "ordinary" => b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef012\n",
        "dense" => b"x\n",
        "long" => b"x",
        _ => panic!("invalid corpus"),
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("allocated.txt");
    let mut file = File::create(&path).unwrap();
    let mut chunk = vec![0; 1 << 20];
    for (index, byte) in chunk.iter_mut().enumerate() {
        *byte = pattern[index % pattern.len()];
    }
    let length = mib.checked_mul(1 << 20).unwrap();
    let mut remaining = length;
    let mut lines = 1u64;
    while remaining > 0 {
        let count = remaining.min(chunk.len() as u64) as usize;
        file.write_all(&chunk[..count]).unwrap();
        lines += chunk[..count].iter().filter(|byte| **byte == b'\n').count() as u64;
        remaining -= count as u64;
        if remaining.is_multiple_of(8 << 20) {
            file.sync_data().unwrap();
        }
    }
    file.sync_all().unwrap();
    drop(file);
    eprintln!(
        "fixture={corpus} bytes={length} logical_lines={lines} generated warm on {}",
        directory.path().display()
    );
    let other = directory.path().join("companion.txt");
    std::fs::write(&other, "Companion tab").unwrap();
    let started = Instant::now();
    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), [&path, &other]);
    let first = if corpus == "ordinary" {
        "0123456789abcdef"
    } else {
        "x"
    };
    process.expect_body(first, Duration::from_secs(120));
    let first_text = started.elapsed();
    process.expect_with_timeout("Ready", Duration::from_secs(120));
    let ready = started.elapsed();
    process.send(b"\x1b[19~");
    process.expect_body("Companion tab", TIMEOUT);
    process.send(b"\x1b[18~");
    process.expect_body(first, TIMEOUT);
    let pid = process.editor_pid();
    let rss = || {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.as_raw().to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let mut sampled_rss = rss();
    let mut latency = Vec::new();
    for count in 1..=40 {
        let event = Instant::now();
        process.send(b"Z");
        process.expect_body(&"Z".repeat(count), TIMEOUT);
        latency.push(event.elapsed().as_secs_f64() * 1000.0);
        sampled_rss = sampled_rss.max(rss());
    }
    process.send(b"\x07");
    process.expect("Go to line[:cell]");
    let middle = if corpus == "long" {
        format!("1:{}", length / 2 + 1)
    } else {
        format!("{}", lines / 2 + 1)
    };
    process.send(middle.as_bytes());
    process.send(b"\rB");
    process.expect_body("B", Duration::from_secs(120));
    sampled_rss = sampled_rss.max(rss());
    process.send(b"\x1b[1;5FC");
    process.expect_body("C", Duration::from_secs(120));
    process.send(b"\x13");
    // Continue navigation and typing while the immutable save is in flight.
    process.send(b"\x1b[1;5HD");
    process.expect_body("D", Duration::from_secs(120));
    process.expect_with_timeout("Saved", Duration::from_secs(120));
    sampled_rss = sampled_rss.max(rss());
    latency.sort_by(f64::total_cmp);
    eprintln!(
        "UI corpus={corpus} MiB={mib} first_text_ms={:.3} ready_ms={:.3} ordinary_samples={} p50_ms={:.3} p95_ms={:.3} max_ms={:.3} sampled_max_editor_RSS_KiB={sampled_rss}",
        first_text.as_secs_f64() * 1000.0,
        ready.as_secs_f64() * 1000.0,
        latency.len(),
        latency[latency.len() / 2],
        latency[latency.len() * 95 / 100],
        latency.last().unwrap()
    );
    let file = File::open(&path).unwrap();
    let mut bytes = [0; 40];
    file.read_exact_at(&mut bytes, 0).unwrap();
    assert_eq!(&bytes, b"ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ");
    assert_eq!(file.metadata().unwrap().len(), length + 42);
    // The later D was not in the captured save, so normal quit must still protect it.
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
    process.report_peak_rss();
}

/// Search/replace over a real larger-than-cache fixture while measuring live input.
#[test]
#[ignore = "search resource benchmark: EDE_BENCH_MIB=1024 EDE_BENCH_CORPUS=ordinary|dense|long"]
fn large_search_and_replace_benchmark() {
    use std::os::unix::fs::FileExt;
    let mib: u64 = std::env::var("EDE_BENCH_MIB")
        .unwrap_or_else(|_| "1024".into())
        .parse()
        .unwrap();
    let corpus = std::env::var("EDE_BENCH_CORPUS").unwrap_or_else(|_| "ordinary".into());
    assert!(mib > 0);
    let pattern: &[u8] = match corpus.as_str() {
        "ordinary" => b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef012\n",
        "dense" => b"x\n",
        "long" => b"x",
        _ => panic!("unknown corpus"),
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("search-benchmark");
    let mut chunk = vec![0; 1 << 20];
    for (index, byte) in chunk.iter_mut().enumerate() {
        *byte = pattern[index % pattern.len()];
    }
    chunk[..10].copy_from_slice(b"SEEK_TOKEN");
    let mut file = File::create(&path).unwrap();
    for index in 0..mib {
        file.write_all(&chunk).unwrap();
        if (index + 1).is_multiple_of(8) {
            file.sync_data().unwrap();
        }
    }
    file.sync_all().unwrap();
    drop(file);
    eprintln!(
        "search fixture_ready corpus={corpus} bytes={} warm",
        mib << 20
    );
    let started = Instant::now();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_body("SEEK_TOKEN", Duration::from_secs(120));
    let first_text = started.elapsed();
    process.expect_with_timeout("Ready", Duration::from_secs(120));
    let ready = started.elapsed();
    let pid = process.editor_pid();
    let rss = || {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.as_raw().to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let mut peak_rss = rss();
    process.send(b"\x06SEEK_TOKEN\r");
    let scan_started = Instant::now();
    process.expect_with_timeout("exact match differs", Duration::from_secs(120));
    assert!(
        process
            .parser
            .screen()
            .contents()
            .contains(&format!("{mib} matches"))
    );
    let scan = scan_started.elapsed();
    process.send(b"\x06\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f");
    process.send(if corpus == "ordinary" { b"0\r" } else { b"x\r" });
    process.send(b"\x1b[17~"); // F6 returns to the live document.
    let mut latency = Vec::new();
    for count in 1..=40 {
        let event = Instant::now();
        process.send(b"Z");
        process.expect_body(&"Z".repeat(count), TIMEOUT);
        latency.push(event.elapsed().as_secs_f64() * 1000.0);
        peak_rss = peak_rss.max(rss());
    }
    let cancel_started = Instant::now();
    process.send(b"\x1b");
    let cancel_deadline = Instant::now() + Duration::from_secs(30);
    let cancellation_observed = loop {
        process.drain();
        let screen = process.parser.screen().contents();
        if screen.contains("Cancelled") {
            break true;
        }
        if !screen.contains("Find:") {
            break false;
        }
        assert!(
            Instant::now() < cancel_deadline,
            "cancel did not settle: {screen}"
        );
        thread::sleep(Duration::from_millis(1));
    };
    let cancellation = cancel_started.elapsed();
    process.send(b"\x08\x7fSEEK_TOKEN\tFOUND_DONE\t\t\t\t\t\t\t\r");
    process.expect("Replace all — confirm scope");
    let replace_started = Instant::now();
    process.send(b"\t\r");
    process.expect_with_timeout(
        &format!("{mib} occurrences processed"),
        Duration::from_secs(180),
    );
    let replacement = replace_started.elapsed();
    peak_rss = peak_rss.max(rss());
    process.send(b"\x1b");
    process.expect_absent("Replace all…");
    process.send(b"\x13");
    process.expect_with_timeout("Saved", Duration::from_secs(120));
    let file = File::open(&path).unwrap();
    assert_eq!(file.metadata().unwrap().len(), (mib << 20) + 40);
    let mut bytes = [0; 10];
    for block in [0, mib / 2, mib - 1] {
        file.read_exact_at(&mut bytes, (block << 20) + 40).unwrap();
        assert_eq!(&bytes, b"FOUND_DONE");
    }
    process.send(b"\x1a");
    process.expect_body("SEEK_TOKEN", TIMEOUT);
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
    process.report_peak_rss();
    latency.sort_by(f64::total_cmp);
    eprintln!(
        "search_ui corpus={corpus} MiB={mib} first_text_ms={:.3} ready_ms={:.3} full_scan_ms={:.3} replace_ms={:.3} cancellation_observed={cancellation_observed} cancellation_ms={:.3} input_p50_ms={:.3} input_p95_ms={:.3} input_max_ms={:.3} sampled_peak_RSS_KiB={peak_rss}",
        first_text.as_secs_f64() * 1000.0,
        ready.as_secs_f64() * 1000.0,
        scan.as_secs_f64() * 1000.0,
        replacement.as_secs_f64() * 1000.0,
        cancellation.as_secs_f64() * 1000.0,
        latency[20],
        latency[38],
        latency[39]
    );
}

/// Pasted path data and immediately following typing/Enter stay in order off the UI thread.
#[test]
fn pasted_save_as_path_and_immediate_submit() {
    let directory = tempfile::tempdir().unwrap();
    let stem = directory.path().join("pasted destination");
    let destination = directory.path().join("pasted destination.txt");
    let mut process = PtyChild::editor();
    process.expect("Ready");
    process.send("emoji 👩\n".as_bytes());
    process.send(b"\x13");
    process.expect("Save As (new path)");
    process.send(b"\x1b[200~");
    process.send(stem.as_os_str().as_encoded_bytes());
    process.send(b"\x1b[201~.txt\r");
    process.expect_status("Saved");
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        "emoji 👩\n".as_bytes()
    );
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A rapid multiline drag, copy and paste preserves ordering without waiting between mouse events.
#[test]
fn text_mouse_drag_copy_paste_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mouse.txt");
    let original = "abc界def\n0123456789\nlast";
    std::fs::write(&path, original).unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("Ready");
    process.send(b"\x1b[<0;3;2M\x1b[<32;4;3M\x1b[<0;5;3m\x03\x1b[1;5F\r\x16\x13");
    let expected = format!("{original}\nc界def\n0123");
    let deadline = Instant::now() + TIMEOUT;
    while std::fs::read(&path).unwrap() != expected.as_bytes() {
        process.drain();
        assert!(
            Instant::now() < deadline,
            "{}",
            process.parser.screen().contents()
        );
        thread::sleep(Duration::from_millis(5));
    }
    process.expect_status("Saved");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Save As refuses an existing destination and offers a new path without dropping the buffer.
#[test]
fn save_as_existing_destination_retains_both_contents() {
    let directory = tempfile::tempdir().unwrap();
    let existing = directory.path().join("existing");
    let fresh = directory.path().join("fresh");
    std::fs::write(&existing, b"external").unwrap();
    let mut process = PtyChild::editor();
    process.expect("Ready");
    process.send(b"mine\x13");
    process.expect("Save As (new path)");
    process.send(existing.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect("work retained");
    assert_eq!(std::fs::read(&existing).unwrap(), b"external");
    process.send(b"\x1bOS");
    process.expect("Save As (new path)");
    process.send(fresh.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(fresh).unwrap(), b"mine");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Separate ingress RSS from document-cache RSS while paste size grows, using fixed input chunks.
#[test]
#[ignore = "paste memory benchmark: run explicitly with EDE_PASTE_MIB"]
fn large_paste_memory_benchmark() {
    let mib: u64 = std::env::var("EDE_PASTE_MIB")
        .unwrap_or_else(|_| "64".into())
        .parse()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("paste");
    let mut process = PtyChild::editor();
    process.expect("Ready");
    let pid = process.editor_pid();
    let rss = || {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.as_raw().to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let baseline = rss();
    let mut ingress_rss = baseline;
    let chunk = vec![b'p'; 64 << 10];
    let started = Instant::now();
    process.send(b"\x1b[200~");
    for index in 0..mib * 16 {
        process.send(&chunk);
        if index.is_multiple_of(128) {
            ingress_rss = ingress_rss.max(rss());
        }
    }
    ingress_rss = ingress_rss.max(rss());
    process.send(b"PASTE_END\x1b[201~");
    process.expect_body("PASTE_END", Duration::from_secs(120));
    let ready_rss = rss();
    process.send(b"\x13");
    process.expect("Save As (new path)");
    process.send(path.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_with_timeout("Saved", Duration::from_secs(120));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), mib * (1 << 20) + 9);
    eprintln!(
        "paste_MiB={mib} baseline_RSS_KiB={baseline} sampled_ingress_max_RSS_KiB={ingress_rss} document_ready_RSS_KiB={ready_rss} receive_render_save_ms={:.3}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
    process.report_peak_rss();
}

/// Isolated tmux acceptance: portable editing/clipboard/paste, guarded saves and real job control.
#[test]
#[ignore = "requires local tmux: run explicitly with an isolated socket"]
fn isolated_tmux_editing_and_suspend_smoke() {
    use std::path::{Path, PathBuf};
    /// This socket belongs only to the test; cleanup cannot reach a user's tmux server.
    struct Server(PathBuf);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(&self.0)
                .arg("kill-server")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    /// Quote fixture paths as one shell argument without interpreting embedded shell syntax.
    fn quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }
    /// Darwin may set transient PENDIN while returning to its interactive shell.
    fn cooked(file: &File) -> Termios {
        let mut attributes = termios::tcgetattr(file).unwrap();
        attributes.local_flags.remove(termios::LocalFlags::PENDIN);
        attributes
    }
    /// Wait for the interactive shell to finish restoring its own line-editing mode.
    fn restored(pane: &File, original: &Termios) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let actual = cooked(pane);
            if actual == *original {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "shell attributes did not settle: {actual:?} != {original:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let server = Server(directory.path().join("tmux.sock"));
    let path = directory.path().join("document");
    std::fs::write(&path, b"first\nsecond").unwrap();
    let mut command = Command::new("tmux");
    command
        .arg("-S")
        .arg(&server.0)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-s",
            "ede-stage3",
            "--",
            "/bin/sh",
            "-i",
        ])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    let mut process = PtyChild::spawn(command);
    process.expect("EDE_TEST_PROMPT>");
    let tty = Command::new("tmux")
        .arg("-S")
        .arg(&server.0)
        .args(["list-panes", "-F", "#{pane_tty}"])
        .output()
        .unwrap();
    assert!(tty.status.success());
    let tty = String::from_utf8(tty.stdout).unwrap();
    let pane = File::open(tty.trim()).unwrap();
    let original = cooked(&pane);
    process.send(
        format!(
            "{} {}\n",
            quote(Path::new(env!("CARGO_BIN_EXE_ede"))),
            quote(&path)
        )
        .as_bytes(),
    );
    process.expect("Ready");
    process.send(b"X");
    process.expect("Xfirst");
    process.send(b"\x13");
    process.expect("Saved");
    assert_eq!(std::fs::read(&path).unwrap(), b"Xfirst\nsecond");
    process.send(b"\x01\x03\x1b[1;5F\r\x16I");
    process.expect("secondI");
    process.send(b"\x1b[200~-external\n\x11TAIL\x1b[201~");
    process.expect("TAIL");
    process.send(b"\x13");
    process.expect("Saved");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"Xfirst\nsecond\nXfirst\nsecondI-external\n\x11TAIL"
    );
    // Exercise the client's actual SGR mouse forwarding, including a release
    // position later than the last drag event, through the isolated tmux server.
    process.send(b"\x1b[1;5H");
    process.expect("Ln 1 Col 1");
    process.send(b"\x1b[<0;1;2M\x1b[<32;2;2M\x1b[<0;3;2m\x03\x1b[1;5F\r\x16\x13");
    process.expect("Saved");
    let expected = b"Xfirst\nsecond\nXfirst\nsecondI-external\n\x11TAIL\nXf";
    let deadline = Instant::now() + TIMEOUT;
    while std::fs::read(&path).unwrap() != expected {
        process.drain();
        assert!(
            Instant::now() < deadline,
            "{}",
            process.parser.screen().contents()
        );
        thread::sleep(Duration::from_millis(5));
    }
    // Pause native capture through the menu and keep it paused through suspend.
    process.send(b"\x1bOQ");
    process.expect("Actions");
    for _ in 0..11 {
        process.send(b"\x1b[B");
    }
    process.send(b"\r\x1bOQ");
    process.expect("Mouse capture: paused");
    process.send(b"\x1b");
    let deadline = Instant::now() + TIMEOUT;
    while process
        .parser
        .screen()
        .contents()
        .contains("Mouse capture: paused")
    {
        process.drain();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    process.send(b"\x16");
    process.expect_body("XfXf", TIMEOUT);
    process.send(b"\x1a\x13");
    process.expect("Saved");
    process.send(b"\x1bOP");
    process.expect("Cmd aliases require");
    process.send(b"\t\r");
    process.expect("EDE_TEST_PROMPT>");
    restored(&pane, &original);
    process.send(b"fg\n");
    process.expect("Cmd aliases require");
    process.send(b"\x1b");
    let deadline = Instant::now() + TIMEOUT;
    while process
        .parser
        .screen()
        .contents()
        .contains("Cmd aliases require")
    {
        process.drain();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    process.send(b"\x11");
    process.expect("EDE_TEST_PROMPT>");
    restored(&pane, &original);
    process.send(b"exit\n");
    assert!(process.wait().success());
}

/// A wheel-then-click burst must not reuse the rows above the new viewport.
#[test]
fn wheel_then_click_rejects_obsolete_cell_map() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wheel.txt");
    let content = "row0\nrow1\nrow2\nrow3\nrow4\n";
    std::fs::write(&path, content).unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("Ready");
    process.expect("row0");
    process.send(b"\x1b[B");
    process.expect_status("Ln 2 Col 1");
    // Scroll three rows down; click the top content row before a fresh screen is available.
    process.send(b"\x1b[<65;1;2M\x1b[<0;1;2M\x1b[<0;1;2m");
    process.send(b"!\x13");
    process.expect_status("Saved");
    let saved = std::fs::read_to_string(path).unwrap();
    assert!(
        saved == "row0\n!row1\nrow2\nrow3\nrow4\n" || saved == "row0\nrow1\nrow2\n!row3\nrow4\n",
        "obsolete cell map changed the row above the viewport: {saved:?}"
    );
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Replacing selected text with identical bytes still ends the selection.
#[test]
fn identical_replacement_moves_caret_without_history_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one");
    std::fs::write(&path, b"a").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_status("Ready");
    process.send(b"\x01ab\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(&path).unwrap(), b"ab");
    process.send(b"\x1a\x1a");
    process.expect_body("a", TIMEOUT);
    process.expect_status("Ln 1 Col 2");
    process.send(b"\x19");
    process.expect_body("ab", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// An open paste can be cancelled without treating Escape or Ctrl+Q bytes as keys.
#[test]
fn cancel_receiving_paste_with_signal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("paste.txt");
    std::fs::write(&path, b"original").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_status("Ready");
    process.send(b"\x1b[200~partial\x1b");
    process.expect_status("Receiving paste | kill -USR1");
    kill(process.editor_pid(), Signal::SIGUSR1).unwrap();
    process.expect_status("Cancelling paste");
    process.send(b"\x11discarded\x1b[201~");
    process.expect_status("Paste cancelled");
    process.send(b"\x1b[200~\x1b[27u\x11!\x1b[201~\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(&path).unwrap(), b"\x1b[27u\x11!original");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Probe backwards focus over CSI-u after the terminal advertises support.
#[test]
fn enhanced_shift_tab_moves_dialog_focus() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("focus.txt");
    std::fs::write(&path, b"original").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_raw("\x1b[?u");
    process.send(b"\x1b[?1u");
    process.expect_raw("\x1b[>1u");
    process.expect("Ready");
    process.send(b"\x1b[14~");
    process.expect("Save As (new path)");
    process.send(b"\x1b[9;2u\rx");
    process.expect_body("xoriginal", TIMEOUT);
    process.send(b"\x13");
    process.expect_status("Saved");
    process.send(b"\x11");
    assert!(process.wait().success());
}
