//! macOS desktop association and a short-lived AppKit pasteboard helper.
use super::transport_process;
use nix::{libc, unistd::Uid};
use std::{
    ffi::{CStr, c_void},
    io::{self, Write},
    mem::{MaybeUninit, size_of},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

fn process(pid: libc::pid_t) -> Option<libc::proc_bsdinfo> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = size_of::<libc::proc_bsdinfo>();
    // SAFETY: libproc initializes the full BSD record on a successful call.
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    (count == size as i32).then(|| unsafe { info.assume_init() })
}

pub(super) fn local_terminal() -> bool {
    let uid = Uid::current().as_raw();
    if !std::fs::metadata("/dev/console").is_ok_and(|console| console.uid() == uid) {
        return false;
    }
    let Some(current) = process(std::process::id() as _) else {
        return false;
    };
    let mut pid = current.pbi_ppid as libc::pid_t;
    let mut shares_tty = false;
    for _ in 0..32 {
        if pid <= 1 {
            break;
        }
        let Some(info) = process(pid) else {
            break;
        };
        let mut bytes = [0; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: libproc receives a writable buffer of the declared size.
        let count =
            unsafe { libc::proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
        if count <= 0 {
            break;
        }
        let Ok(path) = CStr::from_bytes_until_nul(&bytes) else {
            break;
        };
        let path = Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()));
        if transport_process(path) {
            break;
        }
        let login = path == Path::new("/usr/bin/login");
        if info.pbi_uid != uid && !login {
            break;
        }
        if current.e_tdev != u32::MAX && info.e_tdev == current.e_tdev {
            shares_tty = true;
        }
        if matches!(
            path.as_os_str().as_bytes(),
            b"/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"
                | b"/Applications/iTerm.app/Contents/MacOS/iTerm2"
                | b"/Applications/iTerm.app/Contents/MacOS/iTermServer"
                | b"/Applications/iTerm2.app/Contents/MacOS/iTerm2"
                | b"/Applications/iTerm2.app/Contents/MacOS/iTermServer"
        ) {
            return shares_tty;
        }
        pid = info.pbi_ppid as libc::pid_t;
    }
    false
}

type Object = *mut c_void;
type Selector = *const c_void;

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSPasteboardTypeString: Object;
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const i8) -> Object;
    fn sel_registerName(name: *const i8) -> Selector;
    fn objc_msgSend();
}

// AppKit classes and selectors below are fixed, and each call names its method's ABI.
macro_rules! send {
    ($target:expr, $selector:literal, ($($arg:expr => $ty:ty),*) -> $ret:ty) => {{
        let method: unsafe extern "C" fn(Object, Selector $(, $ty)*) -> $ret =
            std::mem::transmute(objc_msgSend as *const ());
        method($target, sel_registerName($selector.as_ptr()) $(, $arg)*)
    }};
}

/// Run one pasteboard operation on the helper's main thread before the editor starts.
pub fn worker(write: bool) -> u8 {
    // SAFETY: AppKit objects live within this pool. The Rust stdin mapping outlives
    // all references to its NSData wrapper and is released on process exit.
    unsafe {
        let pool = send!(objc_getClass(c"NSAutoreleasePool".as_ptr()), c"new", () -> Object);
        let board =
            send!(objc_getClass(c"NSPasteboard".as_ptr()), c"generalPasteboard", () -> Object);
        let result = if board.is_null() {
            1
        } else if write {
            write_board(board)
        } else {
            read_board(board)
        };
        send!(pool, c"drain", () -> ());
        result
    }
}

unsafe fn read_board(board: Object) -> u8 {
    // SAFETY: the returned autoreleased NSData and its contiguous bytes are valid
    // through the enclosing pool. Write requests never fetch pasteboard data.
    unsafe {
        let data = send!(board, c"dataForType:", (NSPasteboardTypeString => Object) -> Object);
        if data.is_null() {
            return 3;
        }
        let length = send!(data, c"length", () -> usize);
        let bytes = send!(data, c"bytes", () -> *const u8);
        if length == 0 {
            return 0;
        }
        if bytes.is_null() || length > isize::MAX as usize {
            return 1;
        }
        let payload = std::slice::from_raw_parts(bytes, length);
        u8::from(io::stdout().lock().write_all(payload).is_err())
    }
}

unsafe fn write_board(board: Object) -> u8 {
    let Ok(stat) = nix::sys::stat::fstat(io::stdin()) else {
        return 1;
    };
    let Ok(length) = usize::try_from(stat.st_size) else {
        return 1;
    };
    if length == 0
        || !nix::sys::stat::SFlag::from_bits_truncate(stat.st_mode)
            .contains(nix::sys::stat::SFlag::S_IFREG)
    {
        return 1;
    }
    // SAFETY: stdin is an immutable, privately staged file owned by the parent.
    // The mapping lives to exit and AppKit does not own or free it.
    unsafe {
        let map = libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            0,
            0,
        );
        if map == libc::MAP_FAILED {
            return 1;
        }
        let data = send!(objc_getClass(c"NSData".as_ptr()), c"dataWithBytesNoCopy:length:freeWhenDone:",
            (map => Object, length => usize, 0 => i8) -> Object);
        if data.is_null() {
            return 1;
        }
        let item = send!(objc_getClass(c"NSPasteboardItem".as_ptr()), c"new", () -> Object);
        let item = send!(item, c"autorelease", () -> Object);
        if send!(item, c"setData:forType:", (data => Object, NSPasteboardTypeString => Object) -> i8)
            == 0
        {
            return 1;
        }
        let items = send!(objc_getClass(c"NSArray".as_ptr()), c"arrayWithObject:", (item => Object) -> Object);
        send!(board, c"clearContents", () -> isize);
        u8::from(send!(board, c"writeObjects:", (items => Object) -> i8) == 0)
    }
}
