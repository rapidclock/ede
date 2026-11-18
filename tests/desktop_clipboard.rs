//! Real X11 selection protocol checks in an isolated Xvfb server.

#![cfg(target_os = "linux")]

use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Process(Child);

impl Process {
    fn finish(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "clipboard process did not exit");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A temporary display with clipboard traffic isolated from the real desktop.
struct Display {
    server: Process,
    name: String,
}

impl Display {
    fn open() -> Self {
        let mut server = Process(
            Command::new("Xvfb")
                .args([
                    "-displayfd",
                    "1",
                    "-screen",
                    "0",
                    "800x600x24",
                    "-nolisten",
                    "tcp",
                ])
                .stdout(Stdio::piped())
                .spawn()
                .expect("Xvfb is required for this test"),
        );
        let pipe = server.0.stdout.take().unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(pipe).read_line(&mut line).unwrap();
            send.send(line.trim().parse::<u32>().unwrap()).unwrap();
        });
        let name = format!(":{}", receive.recv_timeout(TIMEOUT).unwrap());
        Self { server, name }
    }

    fn helper(&self, mode: &str) -> Command {
        let mut helper = Command::new(env!("CARGO_BIN_EXE_ede"));
        helper
            .arg(mode)
            .env("DISPLAY", &self.name)
            .env_remove("XAUTHORITY")
            .env("EDE_X11_PEER", self.server.0.id().to_string());
        helper
    }

    fn copy(&self, bytes: &[u8]) -> Process {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        self.copy_from_file(file)
    }

    fn copy_from_file(&self, file: File) -> Process {
        // The helper serves by offset, independently of the writer's file cursor.
        let mut owner = Process(
            self.helper("--ede-x11-clipboard-write")
                .stdin(file)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut pipe = owner.0.stdout.take().unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut ack = [0];
            let valid = pipe.read_exact(&mut ack).is_ok() && ack == [1];
            send.send(valid).unwrap();
        });
        assert!(
            receive.recv_timeout(TIMEOUT).unwrap(),
            "no ownership acknowledgement"
        );
        owner
    }

    fn paste(&self, expected_peer: bool) -> (ExitStatus, Vec<u8>) {
        let mut command = self.helper("--ede-x11-clipboard-read");
        if !expected_peer {
            command.env("EDE_X11_PEER", "0");
        }
        let mut reader = Process(command.stdout(Stdio::piped()).spawn().unwrap());
        let mut pipe = reader.0.stdout.take().unwrap();
        let drain = thread::spawn(move || {
            let mut output = Vec::new();
            pipe.read_to_end(&mut output).unwrap();
            output
        });
        let status = reader.finish();
        (status, drain.join().unwrap())
    }
}

/// Cover ownership acknowledgement, small and incremental transfers, repeated reads and replacement.
#[test]
#[ignore = "requires Xvfb; run explicitly against an isolated local display"]
fn x11_clipboard_round_trip_and_owner_replacement() {
    let display = Display::open();
    let (status, bytes) = display.paste(true);
    assert_eq!(status.code(), Some(3));
    assert!(bytes.is_empty());

    let marker = "local 界\r\ntext\n".as_bytes();
    let mut small = display.copy(marker);
    assert_eq!(display.paste(true).1, marker);
    let (status, bytes) = display.paste(false);
    assert_eq!(status.code(), Some(1));
    assert!(bytes.is_empty());

    let large = marker.repeat((3 << 20) / marker.len() + 1);
    let mut owner = display.copy(&large);
    assert!(small.finish().success());
    for _ in 0..2 {
        let (status, bytes) = display.paste(true);
        assert!(status.success());
        assert_eq!(bytes, large);
    }
    let replacement = "different 界\nclipboard".as_bytes();
    let _new = display.copy(replacement);
    assert!(owner.finish().success());
    assert_eq!(display.paste(true).1, replacement);
}
