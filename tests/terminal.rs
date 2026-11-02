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

/// Test terminal, retained slave attributes, process, and a bounded diagnostic transcript.
struct PtyChild {
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
        let deadline = Instant::now() + TIMEOUT;
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
        self.master.write_all(bytes).unwrap();
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
    process.expect("No document open");
    assert!(
        !termios::tcgetattr(&process.slave)
            .unwrap()
            .input_flags
            .contains(InputFlags::IXON)
    );
    process.reset_output();
    process.send(b"\x13");
    process.expect_status("No document open");
    process.reset_output();
    process.send(b"\x03");
    process.expect_status("No selection");
    process.reset_output();
    process.send(b"\x1b[200~\x11\x13secret\x1b[201~");
    process.expect_status("No document open");
    assert!(!String::from_utf8_lossy(&process.output).contains("secret"));
    assert!(process.child.try_wait().unwrap().is_none());
    process.send(b"q\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Help is modal and mouse transitions use the rendered target rectangles after resize.
#[test]
fn real_mouse_overlay_and_resize() {
    let mut process = PtyChild::editor();
    process.expect("No document open");
    process.reset_output();
    process.send(b"\x1bOP");
    process.expect("app.quit");
    // Header Quit is underneath the overlay and must not be activated.
    process.send(b"\x1b[<0;74;1M\x1b[<0;74;1m\x13");
    process.expect_status("No document open");
    assert!(process.child.try_wait().unwrap().is_none());
    process.reset_output();
    process.resize(20, 4);
    process.expect("Resize");
    process.reset_output();
    process.resize(80, 24);
    process.expect("app.quit");
    process.send(b"\x1b");
    // Wait for the overlay to disappear before sending another escape-prefixed sequence.
    let deadline = Instant::now() + TIMEOUT;
    while process.parser.screen().contents().contains("app.quit") {
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
        process.expect("No document open");
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
        // The interactive parent supplies job control and stays alive after ede exits,
        // allowing exact post-exit termios capture before Darwin revokes the slave PTY.
        let mut command = Command::new("/bin/sh");
        command.args([
            "-i",
            "-c",
            "if [ \"$1\" = unset ]; then unset TERM; else TERM=$2; export TERM; fi; /bin/sh -c 'printf \"EDE_PID:%s\\n\" \"$$\"; exec \"$1\"' ede-child \"$3\"; result=$?; printf '\\nEDE_EXIT:%s\\n' \"$result\"; read -r completion; exit \"$result\"",
            "ede-term-test",
            if term.is_none() { "unset" } else { "set" },
            term.unwrap_or(""),
            env!("CARGO_BIN_EXE_ede"),
        ]);
        let mut child = Self::spawn(command);
        child.keep_alive = true;
        child
    }
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
    process.expect("No document open");
    let foreground = process.editor_pid();
    process.reset_output();
    process.send(b"\x1bOP");
    process.expect("app.quit");
    process.send(b"\t\r");
    process.expect_raw("EDE_TEST_PROMPT>");
    process.assert_restored();
    process.resize(60, 16);
    process.reset_output();
    process.send(b"fg\n");
    process.expect("app.quit");
    assert!(kill(foreground, None).is_ok());
    process.reset_output();
    // External SIGTSTP follows the same cleanup route as the help action.
    kill(foreground, Signal::SIGTSTP).unwrap();
    process.expect_raw("EDE_TEST_PROMPT>");
    process.assert_restored();
    process.reset_output();
    process.send(b"fg\n");
    process.expect("app.quit");
    process.reset_output();
    process.send(b"\x11");
    process.expect_raw("EDE_TEST_PROMPT>");
    process.assert_restored();
    process.send(b"exit\n");
    assert!(process.wait().success());
}
