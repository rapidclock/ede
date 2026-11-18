//! Associate a local Ubuntu GUI session with the PTY and its Xorg or Xwayland endpoint.
use super::{same_env, transport_process};
use nix::{libc, unistd::Uid};
use std::{
    ffi::{CStr, CString, OsString, c_char, c_void},
    fs,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStringExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    executable: PathBuf,
    session: String,
    display: OsString,
    authority: Option<OsString>,
    peer: i32,
    terminal: u32,
}

const TERMINALS: &[&str] = &[
    "/usr/libexec/gnome-terminal-server",
    "/usr/lib/gnome-terminal/gnome-terminal-server",
    "/usr/libexec/ptyxis-agent",
    "/usr/bin/kgx",
    "/usr/bin/ptyxis",
    "/usr/bin/xterm",
];

fn stat(pid: u32) -> Option<(u32, i32)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace().skip(1);
    let parent = fields.next()?.parse().ok()?;
    let tty = fields.nth(2)?.parse().ok()?;
    Some((parent, tty))
}

fn owns_master(pid: u32, tty: i32) -> bool {
    let index = libc::minor(tty as libc::dev_t);
    let pty_manager = Path::new("/dev/pts/ptmx");
    fs::read_dir(format!("/proc/{pid}/fdinfo")).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let descriptor = entry.file_name();
            if !fs::read_link(Path::new(&format!("/proc/{pid}/fd")).join(&descriptor))
                .is_ok_and(|target| target == pty_manager || target == Path::new("/dev/ptmx"))
            {
                return false;
            }
            fs::read_to_string(entry.path()).is_ok_and(|info| {
                info.lines().any(|line| {
                    line.strip_prefix("tty-index:")
                        .is_some_and(|value| value.trim().parse::<u32>() == Ok(index))
                })
            })
        })
    })
}

fn terminal_process(uid: u32) -> Option<u32> {
    let (mut pid, tty) = stat(std::process::id())?;
    if tty == 0 || !(136..=143).contains(&libc::major(tty as libc::dev_t)) {
        return None;
    }
    for _ in 0..32 {
        if pid <= 1 || fs::metadata(format!("/proc/{pid}")).ok()?.uid() != uid {
            return None;
        }
        let executable = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        if transport_process(&executable) {
            return None;
        }
        if TERMINALS.iter().any(|path| executable == Path::new(path)) && owns_master(pid, tty) {
            return Some(pid);
        }
        pid = stat(pid)?.0;
    }
    None
}

fn process_environment(pid: u32) -> Option<Vec<(OsString, OsString)>> {
    let env = fs::read(format!("/proc/{pid}/environ")).ok()?;
    Some(
        env.split(|byte| *byte == 0)
            .filter_map(|entry| {
                let end = entry.iter().position(|byte| *byte == b'=')?;
                Some((
                    OsString::from_vec(entry[..end].to_vec()),
                    OsString::from_vec(entry[end + 1..].to_vec()),
                ))
            })
            .collect(),
    )
}

fn peer(path: &std::ffi::OsStr) -> Option<(i32, u32)> {
    let bytes = path.as_encoded_bytes();
    // SAFETY: Linux Unix-domain addresses and peer credentials are plain C records.
    unsafe {
        let mut address: libc::sockaddr_un = std::mem::zeroed();
        if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
            return None;
        }
        address.sun_family = libc::AF_UNIX as _;
        for (target, &byte) in address.sun_path.iter_mut().zip(bytes) {
            *target = byte as c_char;
        }
        let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
        let fd = libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        );
        if fd < 0 {
            return None;
        }
        let fd = OwnedFd::from_raw_fd(fd);
        if libc::connect(fd.as_raw_fd(), (&raw const address).cast(), length as _) != 0 {
            return None;
        }
        socket_peer(fd.as_raw_fd())
    }
}

/// Read the credentials of the actual connection before sending clipboard data.
pub(super) fn socket_peer(fd: RawFd) -> Option<(i32, u32)> {
    // SAFETY: getsockopt writes one ucred to an appropriately sized record.
    unsafe {
        let mut credentials: libc::ucred = std::mem::zeroed();
        let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let result = libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut size,
        );
        (result == 0 && size as usize == std::mem::size_of::<libc::ucred>())
            .then_some((credentials.pid, credentials.uid))
    }
}

fn x_peer(number: &str) -> Option<(i32, u32)> {
    let path = format!("/tmp/.X11-unix/X{number}");
    peer(path.as_ref())
}

fn gnome_shell(pid: i32, session: &str, login: &Login) -> bool {
    if !fs::read_link(format!("/proc/{pid}/exe"))
        .is_ok_and(|path| path == Path::new("/usr/bin/gnome-shell"))
    {
        return false;
    }
    if let Some(owner) = login.pid_session(pid as u32) {
        return owner == session;
    }
    // GNOME normally runs its compositor in a systemd user service outside
    // the logind scope. A nested GNOME Shell launched from the terminal does not.
    fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .is_ok_and(|groups| compositor_service(&groups))
}

fn compositor_service(groups: &str) -> bool {
    groups.lines().any(|group| {
        group.ends_with("/session.slice/org.gnome.Shell@wayland.service")
            || group.ends_with("/session.slice/org.gnome.Shell@ubuntu.service")
    })
}

fn x_display(display: &std::ffi::OsStr) -> Option<(&str, &str)> {
    let display = display.to_str()?;
    let number = display
        .strip_prefix(':')?
        .strip_suffix(".0")
        .unwrap_or_else(|| &display[1..]);
    (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some((display, number))
}

impl Endpoint {
    pub(super) fn local(executable: &Path) -> Option<Self> {
        let uid = Uid::current().as_raw();
        let terminal = terminal_process(uid)?;
        let terminal_env = process_environment(terminal)?;
        let login = Login::open()?;
        let session = login.uid_display(uid)?;
        if login
            .pid_session(terminal)
            .is_some_and(|owner| owner != session)
            || !login.flag(c"sd_session_is_active", &session)?
            || login.flag(c"sd_session_is_remote", &session)?
            || login.property(c"sd_session_get_seat", &session)? != "seat0"
        {
            return None;
        }
        let display = same_env(&terminal_env, "DISPLAY")?;
        let (_, number) = x_display(&display)?;
        let authority = same_env(&terminal_env, "XAUTHORITY");
        if std::env::var_os("XAUTHORITY") != authority
            || authority
                .as_ref()
                .is_some_and(|path| !Path::new(path).is_absolute())
        {
            return None;
        }
        let authority = authority.or_else(|| {
            let home = PathBuf::from(same_env(&terminal_env, "HOME")?);
            home.is_absolute()
                .then(|| home.join(".Xauthority").into_os_string())
        });
        let (peer_pid, peer_uid) = x_peer(number)?;
        match login.property(c"sd_session_get_type", &session)?.as_str() {
            "x11" => {
                let declared = login.property(c"sd_session_get_display", &session)?;
                if x_display(declared.as_ref())?.1 != number
                    || !matches!(peer_uid, 0) && peer_uid != uid
                    || !fs::read_link(format!("/proc/{peer_pid}/exe")).is_ok_and(|path| {
                        ["/usr/lib/xorg/Xorg", "/usr/bin/Xorg"]
                            .iter()
                            .any(|server| path == Path::new(server))
                    })
                {
                    return None;
                }
            }
            "wayland" if peer_uid == uid => {
                let runtime = same_env(&terminal_env, "XDG_RUNTIME_DIR")?;
                let wayland = same_env(&terminal_env, "WAYLAND_DISPLAY")?;
                let runtime_path = Path::new(&runtime);
                let socket_name = wayland.to_str()?;
                if runtime_path != Path::new(&format!("/run/user/{uid}"))
                    || !socket_name.strip_prefix("wayland-").is_some_and(|number| {
                        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                    })
                    || !fs::symlink_metadata(runtime_path).is_ok_and(|metadata| {
                        metadata.uid() == uid && metadata.is_dir() && metadata.mode() & 0o077 == 0
                    })
                {
                    return None;
                }
                let (compositor, compositor_uid) = peer(runtime_path.join(wayland).as_os_str())?;
                let same_server = peer_pid == compositor
                    || (fs::read_link(format!("/proc/{peer_pid}/exe"))
                        .is_ok_and(|path| path == Path::new("/usr/bin/Xwayland"))
                        && stat(peer_pid as u32)
                            .is_some_and(|(parent, _)| parent == compositor as u32));
                if compositor_uid != uid
                    || !same_server
                    || !gnome_shell(compositor, &session, &login)
                {
                    return None;
                }
            }
            _ => return None,
        }
        // Keep the exact local display and credentials proved above. No inherited
        // toolkit, Wayland socket, or forwarded-display variables reach the helper.
        Some(Self {
            executable: executable.to_owned(),
            session,
            display,
            authority,
            peer: peer_pid,
            terminal,
        })
    }

    pub(super) fn valid(&self) -> bool {
        Self::local(&self.executable).as_ref() == Some(self)
    }

    pub(super) fn command(&self, write: bool) -> Command {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new(&self.executable);
        command
            .process_group(0)
            .env_clear()
            .env("DISPLAY", &self.display)
            .env("EDE_X11_PEER", self.peer.to_string())
            .stderr(Stdio::null())
            .arg(if write {
                "--ede-x11-clipboard-write"
            } else {
                "--ede-x11-clipboard-read"
            });
        if let Some(authority) = &self.authority {
            command.env("XAUTHORITY", authority);
        }
        command
    }
}

/// Bind the stable logind APIs without imposing a systemd linker dependency on other Linux systems.
struct Login(*mut c_void);

impl Login {
    fn open() -> Option<Self> {
        // SAFETY: fixed library name. The handle owns these symbols until Drop.
        let library = unsafe {
            libc::dlopen(
                c"libsystemd.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        };
        (!library.is_null()).then_some(Self(library))
    }

    fn transport_string(&self, call: impl FnOnce(*mut *mut c_char) -> i32) -> Option<String> {
        let mut result = std::ptr::null_mut();
        if call(&mut result) < 0 || result.is_null() {
            return None;
        }
        // SAFETY: sd-login returns a malloc-owned NUL-terminated result on success.
        unsafe {
            let text = CStr::from_ptr(result).to_str().ok().map(str::to_owned);
            libc::free(result.cast());
            text
        }
    }

    fn uid_display(&self, uid: u32) -> Option<String> {
        // SAFETY: this symbol has the sd_uid_get_display ABI.
        unsafe {
            let symbol = libc::dlsym(self.0, c"sd_uid_get_display".as_ptr());
            if symbol.is_null() {
                return None;
            }
            let function: unsafe extern "C" fn(libc::uid_t, *mut *mut c_char) -> i32 =
                std::mem::transmute(symbol);
            self.transport_string(|value| function(uid, value))
        }
    }

    fn pid_session(&self, pid: u32) -> Option<String> {
        // SAFETY: this symbol has the sd_pid_get_session ABI.
        unsafe {
            let symbol = libc::dlsym(self.0, c"sd_pid_get_session".as_ptr());
            if symbol.is_null() {
                return None;
            }
            let function: unsafe extern "C" fn(libc::pid_t, *mut *mut c_char) -> i32 =
                std::mem::transmute(symbol);
            self.transport_string(|value| function(pid as libc::pid_t, value))
        }
    }

    fn property(&self, name: &CStr, session: &str) -> Option<String> {
        let session = CString::new(session).ok()?;
        // SAFETY: callers supply only sd_session_get_* string property symbols.
        unsafe {
            let symbol = libc::dlsym(self.0, name.as_ptr());
            if symbol.is_null() {
                return None;
            }
            let function: unsafe extern "C" fn(*const c_char, *mut *mut c_char) -> i32 =
                std::mem::transmute(symbol);
            self.transport_string(|value| function(session.as_ptr(), value))
        }
    }

    fn flag(&self, name: &CStr, session: &str) -> Option<bool> {
        let session = CString::new(session).ok()?;
        // SAFETY: callers supply only sd_session_is_* boolean property symbols.
        unsafe {
            let symbol = libc::dlsym(self.0, name.as_ptr());
            if symbol.is_null() {
                return None;
            }
            let function: unsafe extern "C" fn(*const c_char) -> i32 = std::mem::transmute(symbol);
            match function(session.as_ptr()) {
                value if value < 0 => None,
                value => Some(value != 0),
            }
        }
    }
}

impl Drop for Login {
    fn drop(&mut self) {
        // SAFETY: no symbol may outlive this handle.
        unsafe { libc::dlclose(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ubuntu names the desktop session's GNOME Shell instance after its session.
    #[test]
    fn ubuntu_session_compositor_is_distinct_from_nested_shells() {
        let scope = "0::/user.slice/user-1000.slice/user@1000.service";
        assert!(compositor_service(&format!(
            "{scope}/session.slice/org.gnome.Shell@ubuntu.service"
        )));
        assert!(compositor_service(&format!(
            "{scope}/session.slice/org.gnome.Shell@wayland.service"
        )));
        assert!(!compositor_service(&format!(
            "{scope}/app.slice/org.gnome.Shell@ubuntu.service"
        )));
        assert!(!compositor_service(&format!(
            "{scope}/session.slice/nested-shell.scope"
        )));
    }
}
