//! Native clipboard transport, admitted only for a positively identified desktop.
use crate::{document::Cancellation, spool::Spool};
use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    unistd::Uid,
};
use std::{
    env,
    io::{self, Read},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::worker as macos_worker;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
pub use x11::worker as x11_worker;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Probed endpoint, independent of the editor's retained clipboard.
#[derive(Debug)]
pub(crate) enum Backend {
    #[cfg(target_os = "macos")]
    Mac(std::path::PathBuf),
    #[cfg(target_os = "linux")]
    Linux(linux::Endpoint),
    #[cfg(test)]
    Test(Arc<dyn TestBackend>),
}

#[cfg(test)]
pub(crate) trait TestBackend: std::fmt::Debug + Send + Sync {
    fn write(&self, spool: &Spool, cancel: &Cancellation) -> Result<(), &'static str>;
    fn read(&self, cancel: &Cancellation) -> Result<Spool, &'static str>;
}

/// Authority is revoked when the session, selection, or explicit command changes.
#[derive(Clone, Debug)]
pub(crate) struct Access {
    pub backend: Arc<Backend>,
    pub cancel: Cancellation,
    pub active: Arc<Mutex<usize>>,
}

impl Access {
    /// Register before checking cancellation so suspension cannot overtake a helper launch.
    pub fn begin(&self) -> InFlight<'_> {
        *self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner()) += 1;
        InFlight(&self.active)
    }
}

/// Release only after the helper has been retired or acknowledged a desktop handoff.
pub(crate) struct InFlight<'a>(&'a Mutex<usize>);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) -= 1;
    }
}

/// Explicit signals of a transport disqualify a native route even with a local display.
fn transport_hint() -> bool {
    [
        "SSH_CONNECTION",
        "SSH_CLIENT",
        "SSH_TTY",
        "TMUX",
        "TMUX_PANE",
        "MOSH_IP",
        "MOSH_PORT",
        "ET_VERSION",
        "ET_SESSION",
        "WT_SESSION",
        "WSL_INTEROP",
        "WSL_DISTRO_NAME",
        "STY",
    ]
    .iter()
    .any(|key| env::var_os(key).is_some_and(|value| !value.is_empty()))
        || env::var_os("TERM").is_some_and(|term| {
            let term = term.as_encoded_bytes();
            term.starts_with(b"screen") || term.starts_with(b"tmux")
        })
}

/// Multiplexers and login servers may reset environment variables before running the shell.
fn transport_process(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.as_encoded_bytes();
        [
            b"ssh".as_slice(),
            b"mosh",
            b"tmux",
            b"screen",
            b"etserver",
            b"wsl",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
            || name == b"et"
    })
}

/// Discover the endpoint without contacting any pasteboard.
pub(crate) fn discover(executable: Option<&Path>) -> Option<Arc<Backend>> {
    if transport_hint() || Uid::current() != Uid::effective() || Uid::current().is_root() {
        return None;
    }
    #[cfg(target_os = "macos")]
    let backend = executable
        .filter(|_| macos::local_terminal())
        .map(|path| Backend::Mac(path.to_owned()));
    #[cfg(target_os = "linux")]
    let backend = {
        executable
            .and_then(linux::Endpoint::local)
            .map(Backend::Linux)
    };
    backend.map(Arc::new)
}

impl Backend {
    fn valid(&self) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            Self::Mac(_) => macos::local_terminal(),
            #[cfg(target_os = "linux")]
            Self::Linux(endpoint) => endpoint.valid(),
            #[cfg(test)]
            Self::Test(_) => true,
        }
    }

    fn command(&self, write: bool) -> Command {
        match self {
            #[cfg(target_os = "macos")]
            Self::Mac(executable) => {
                let mut command = Command::new(executable);
                command.arg(if write {
                    "--ede-pasteboard-write"
                } else {
                    "--ede-pasteboard-read"
                });
                command
            }
            #[cfg(target_os = "linux")]
            Self::Linux(endpoint) => endpoint.command(write),
            #[cfg(test)]
            Self::Test(_) => unreachable!(),
        }
    }

    pub fn write(&self, spool: &Spool, cancel: &Cancellation) -> Result<(), &'static str> {
        if cancel.is_cancelled() {
            return Err("desktop copy cancelled");
        }
        if !self.valid() {
            return Err("desktop session changed");
        }
        if cancel.is_cancelled() {
            return Err("desktop copy cancelled");
        }
        #[cfg(test)]
        if let Self::Test(backend) = self {
            return backend.write(spool, cancel);
        }
        let input = spool
            .input_file()
            .map_err(|_| "desktop copy staging failed")?;
        #[cfg(target_os = "linux")]
        {
            start_owner(self.command(true).stdin(input), cancel)
        }
        #[cfg(target_os = "macos")]
        {
            wait_for_pasteboard_write(self.command(true).stdin(input), cancel)
        }
    }

    pub fn read(&self, cancel: &Cancellation) -> Result<Spool, &'static str> {
        if cancel.is_cancelled() {
            return Err("Desktop paste cancelled");
        }
        if !self.valid() {
            return Err("Desktop session changed");
        }
        if cancel.is_cancelled() {
            return Err("Desktop paste cancelled");
        }
        #[cfg(test)]
        if let Self::Test(backend) = self {
            return backend.read(cancel);
        }
        let mut child = self
            .command(false)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "Desktop paste helper unavailable")?;
        let deadline = Instant::now() + TIMEOUT;
        let result = (|| {
            let mut stdout = child.stdout.take().expect("piped clipboard");
            fcntl(&stdout, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))
                .map_err(|_| "Desktop clipboard read failed")?;
            let mut spool = Spool::new().map_err(|_| "Could not stage desktop text")?;
            let mut buffer = [0; 64 << 10];
            loop {
                if cancel.is_cancelled() {
                    return Err("Desktop paste cancelled");
                }
                if Instant::now() >= deadline {
                    return Err("Desktop clipboard read timed out");
                }
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => spool
                        .append(&buffer[..count])
                        .map_err(|_| "Could not stage desktop text")?,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return Err("Desktop clipboard read failed"),
                }
            }
            let exit =
                wait(&mut child, cancel, deadline).map_err(|failure| failure.paste_message())?;
            if !exit.success() {
                return Err(if exit.code() == Some(3) {
                    "No plain text on desktop clipboard"
                } else {
                    "Desktop clipboard read failed"
                });
            }
            if spool.len == 0 {
                return Err("Desktop clipboard is empty");
            }
            Ok(spool)
        })();
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }
}

#[cfg(target_os = "macos")]
fn wait_for_pasteboard_write(
    command: &mut Command,
    cancel: &Cancellation,
) -> Result<(), &'static str> {
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "desktop copy helper unavailable")?;
    let result = wait(&mut child, cancel, Instant::now() + TIMEOUT)
        .map_err(|failure| failure.copy_message())?;
    result
        .success()
        .then_some(())
        .ok_or("desktop clipboard write failed")
}

/// A successful X11 owner keeps serving repeated requests after ede exits.
#[cfg(target_os = "linux")]
fn start_owner(command: &mut Command, cancel: &Cancellation) -> Result<(), &'static str> {
    let mut child = command
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| "desktop copy helper unavailable")?;
    let result = (|| {
        let mut stdout = child.stdout.take().expect("piped handoff status");
        fcntl(&stdout, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))
            .map_err(|_| "desktop clipboard write failed")?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let mut ack = [0];
            match stdout.read(&mut ack) {
                Ok(1) if ack == [1] => return Ok(()),
                Ok(_) => return Err("desktop clipboard write failed"),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err("desktop clipboard write failed"),
            }
            if cancel.is_cancelled() {
                return Err("desktop copy cancelled");
            }
            if Instant::now() >= deadline {
                return Err("desktop clipboard write timed out");
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_ok() {
        let _ = thread::Builder::new()
            .name("ede-clipboard-owner".into())
            .spawn(move || child.wait());
    } else {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

enum WaitFailure {
    Io,
    Cancelled,
    TimedOut,
}

impl WaitFailure {
    #[cfg(target_os = "macos")]
    fn copy_message(&self) -> &'static str {
        match self {
            Self::Io => "desktop clipboard write failed",
            Self::Cancelled => "desktop copy cancelled",
            Self::TimedOut => "desktop clipboard write timed out",
        }
    }

    fn paste_message(&self) -> &'static str {
        match self {
            Self::Io => "Desktop clipboard read failed",
            Self::Cancelled => "Desktop paste cancelled",
            Self::TimedOut => "Desktop clipboard read timed out",
        }
    }
}

fn wait(
    child: &mut std::process::Child,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<ExitStatus, WaitFailure> {
    let failure = loop {
        match child.try_wait() {
            // Prefer a definitive success even if cancellation happened before polling.
            Ok(Some(result)) => return Ok(result),
            Ok(None) => {}
            Err(_) => break WaitFailure::Io,
        }
        if cancel.is_cancelled() {
            break WaitFailure::Cancelled;
        }
        if Instant::now() >= deadline {
            break WaitFailure::TimedOut;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let _ = child.kill();
    match child.wait() {
        Ok(status) if status.success() => Ok(status),
        _ => Err(failure),
    }
}

/// Environment-backed paths must be absolute and come from the identified desktop.
#[cfg(target_os = "linux")]
fn same_env(
    settings: &[(std::ffi::OsString, std::ffi::OsString)],
    key: &str,
) -> Option<std::ffi::OsString> {
    let trusted = settings.iter().find(|(name, _)| name == key)?.1.clone();
    (env::var_os(key).as_ref() == Some(&trusted)).then_some(trusted)
}
