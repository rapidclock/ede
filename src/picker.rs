//! Bounded directory pages and one revocable filesystem worker.

use crate::{document::Cancellation, editor::absolute, text::safe};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

const PAGE_SIZE: usize = 128;

/// Direct native filename, sorted with directories before all other entries.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Entry {
    file: bool,
    pub name: OsString,
}

impl Entry {
    /// Escape terminal control characters and mark browseable directories.
    pub fn label(&self) -> String {
        format!(
            "{}{}",
            safe(&self.name.to_string_lossy()),
            if self.file { "" } else { "/" }
        )
    }
}

/// One sorted page and the boundaries available on either side of it.
#[derive(Clone, Debug)]
pub(crate) struct Page {
    pub directory: PathBuf,
    pub entries: Vec<Entry>,
    pub previous: bool,
    pub next: bool,
}

/// A directory scan begins at an exclusive native filename boundary.
#[derive(Clone, Debug)]
pub(crate) enum Browse {
    First,
    Next(Entry),
    Previous(Entry),
}

/// A revocable directory or path operation, with the original native spelling.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub generation: u64,
    pub path: PathBuf,
    /// A listing request skips file activation, including for a vanished directory.
    pub browse: Option<Browse>,
}

/// Ready-to-display page or a regular file to pass to document preparation.
pub(crate) enum Outcome {
    Page(Page),
    File(PathBuf),
}

/// A completed operation for the dialog's current generation.
pub(crate) struct Reply {
    pub generation: u64,
    pub result: Result<Outcome, String>,
}

#[derive(Default)]
struct Mailbox {
    queued: Option<(Request, Cancellation)>,
    active: Option<Cancellation>,
    reply: Option<Reply>,
    stop: bool,
}

/// Replacing a queued request cancels the running operation, even if it is inside I/O.
pub(crate) struct Picker {
    shared: Arc<(Mutex<Mailbox>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl Picker {
    /// Start one filesystem worker with a replaceable request and response slot.
    pub fn new() -> io::Result<Self> {
        Self::with_reader(read)
    }

    fn with_reader(
        reader: impl Fn(&Request, &Cancellation) -> io::Result<Outcome> + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let mailbox = shared.clone();
        let worker = thread::Builder::new()
            .name("ede-picker".into())
            .spawn(move || {
                let (lock, wake) = &*mailbox;
                loop {
                    let mut slot = wake
                        .wait_while(lock.lock().unwrap(), |m| m.queued.is_none() && !m.stop)
                        .unwrap();
                    if slot.stop {
                        break;
                    }
                    let (request, cancel) = slot.queued.take().expect("picker request");
                    slot.active = Some(cancel.clone());
                    drop(slot);
                    let result = reader(&request, &cancel).map_err(|error| {
                        let path = safe(&request.path.to_string_lossy());
                        format!("{error} — {path}")
                    });
                    let mut slot = lock.lock().unwrap();
                    slot.active = None;
                    if !slot.stop && !cancel.is_cancelled() {
                        slot.reply = Some(Reply {
                            generation: request.generation,
                            result,
                        });
                    }
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    /// Give this request sole authority to update the dialog.
    pub fn request(&self, request: Request) {
        let (lock, wake) = &*self.shared;
        let mut slot = lock.lock().unwrap();
        Self::revoke(&mut slot);
        slot.queued = Some((request, Cancellation::default()));
        wake.notify_one();
    }

    /// Revoke pending replies without waiting for a filesystem call.
    pub fn cancel(&self) {
        Self::revoke(&mut self.shared.0.lock().unwrap());
    }

    fn revoke(slot: &mut Mailbox) {
        if let Some(active) = &slot.active {
            active.cancel();
        }
        slot.queued = None;
        slot.reply = None;
    }

    /// Take a completed reply without waiting.
    pub fn poll(&self) -> Option<Reply> {
        self.shared.0.lock().unwrap().reply.take()
    }

    /// Stop admissions and return the worker to join after terminal restoration.
    pub fn shutdown(mut self) -> Option<JoinHandle<()>> {
        self.stop();
        self.worker.take()
    }

    fn stop(&self) {
        let (lock, wake) = &*self.shared;
        let mut slot = lock.lock().unwrap();
        Self::revoke(&mut slot);
        slot.stop = true;
        wake.notify_one();
    }
}

impl Drop for Picker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Classify activation by target type while retaining the supplied path.
fn read(request: &Request, cancel: &Cancellation) -> io::Result<Outcome> {
    let path = &request.path;
    let browse = if let Some(browse) = &request.browse {
        browse
    } else {
        let kind = fs::metadata(path)?.file_type();
        if kind.is_file() {
            return Ok(Outcome::File(path.clone()));
        }
        if !kind.is_dir() {
            return Err(io::Error::other("Choose a regular file or a directory"));
        }
        &Browse::First
    };
    Ok(Outcome::Page(list(path, browse, cancel)?))
}

/// Scan with at most 128 retained names, using a boundary to reach any page.
fn list(path: &Path, browse: &Browse, cancel: &Cancellation) -> io::Result<Page> {
    let mut names = BTreeSet::new();
    let mut previous = false;
    let mut next = false;
    let backwards = matches!(browse, Browse::Previous(_));
    for entry in fs::read_dir(path)? {
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let entry = entry?;
        let kind = entry.file_type()?;
        // Keep broken links selectable so activation can report the attempted path.
        let dir = kind.is_dir()
            || (kind.is_symlink()
                && fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir()));
        let item = Entry {
            file: !dir,
            name: entry.file_name(),
        };
        match browse {
            Browse::Next(anchor) if &item <= anchor => {
                previous = true;
                continue;
            }
            Browse::Previous(anchor) if &item >= anchor => {
                next = true;
                continue;
            }
            _ => {}
        }
        names.insert(item);
        if names.len() > PAGE_SIZE {
            if backwards {
                names.pop_first();
                previous = true;
            } else {
                names.pop_last();
                next = true;
            }
        }
    }
    if names.is_empty() && !matches!(browse, Browse::First) {
        return Err(io::Error::other(
            "Directory changed; this page is empty. Go up or enter a path to refresh.",
        ));
    }
    Ok(Page {
        directory: path.to_owned(),
        entries: names.into_iter().collect(),
        previous,
        next,
    })
}

/// View state retained when navigation or path activation fails.
#[derive(Clone, Debug)]
pub(crate) struct Dialog {
    pub follow: bool,
    pub directory: PathBuf,
    pub page: Option<Page>,
    pub highlight: usize,
    pub field: String,
    pub loading: bool,
    pub generation: u64,
    pub requested_file: Option<PathBuf>,
    pub revision: u64,
}

impl Dialog {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            follow: false,
            directory,
            page: None,
            highlight: 0,
            field: String::new(),
            loading: false,
            generation: 0,
            requested_file: None,
            revision: 0,
        }
    }

    pub fn row(&self, index: usize) -> Option<&Entry> {
        self.page.as_ref()?.entries.get(index)
    }

    pub fn count(&self) -> usize {
        self.page.as_ref().map_or(0, |page| page.entries.len())
    }

    pub fn scroll(&mut self, delta: isize) {
        self.highlight = self
            .highlight
            .saturating_add_signed(delta)
            .min(self.count().saturating_sub(1));
    }

    pub fn field_path(&self) -> PathBuf {
        absolute(&self.directory, Path::new(&self.field))
    }

    pub fn parent(&self) -> Option<PathBuf> {
        self.directory.parent().map(Path::to_owned)
    }

    pub fn page_request(&self, backwards: bool) -> Option<Browse> {
        let page = self.page.as_ref()?;
        if backwards && page.previous {
            page.entries.first().cloned().map(Browse::Previous)
        } else if !backwards && page.next {
            page.entries.last().cloned().map(Browse::Next)
        } else {
            None
        }
    }

    pub fn accept(&mut self, page: Page) {
        self.directory.clone_from(&page.directory);
        self.page = Some(page);
        self.highlight = 0;
        self.revision = self.revision.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::symlink,
        sync::mpsc,
        time::{Duration, Instant},
    };

    /// Keyset pages traverse all entries and classify symlink directories by target.
    #[test]
    fn native_directory_pages_and_vanishing_entries() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path();
        fs::create_dir(directory.join("z-dir")).unwrap();
        symlink(directory.join("z-dir"), directory.join("a-link")).unwrap();
        for n in 0..260 {
            fs::write(directory.join(format!("f-{n:03}")), "").unwrap();
        }
        fs::write(directory.join(".hidden\tfile"), "hidden").unwrap();
        symlink(directory.join("gone"), directory.join("zz-broken")).unwrap();
        nix::unistd::mkfifo(&directory.join("pipe"), nix::sys::stat::Mode::S_IRUSR).unwrap();
        let cancel = Cancellation::default();
        let first = list(directory, &Browse::First, &cancel).unwrap();
        assert_eq!(first.entries.len(), PAGE_SIZE);
        assert_eq!(first.entries[0].label(), "a-link/");
        assert_eq!(first.entries[1].label(), "z-dir/");
        assert!(!first.entries[2].label().contains('\t'));
        let second = list(
            directory,
            &Browse::Next(first.entries.last().unwrap().clone()),
            &cancel,
        )
        .unwrap();
        assert!(second.previous && second.next);
        let previous = list(
            directory,
            &Browse::Previous(second.entries[0].clone()),
            &cancel,
        )
        .unwrap();
        assert_eq!(previous.entries, first.entries);
        let boundary = Browse::Next(second.entries.last().unwrap().clone());
        let last = list(directory, &boundary, &cancel).unwrap();
        assert!(last.previous && !last.next);
        assert_eq!(
            first.entries.len() + second.entries.len() + last.entries.len(),
            265
        );
        for item in &last.entries {
            fs::remove_file(directory.join(&item.name)).unwrap();
        }
        assert!(list(directory, &boundary, &cancel).is_err());
        for name in ["pipe", "zz-broken", "gone"] {
            let request = Request {
                generation: 1,
                path: directory.join(name),
                browse: None,
            };
            assert!(read(&request, &cancel).is_err());
        }
        let request = Request {
            generation: 1,
            path: directory.join("a-link"),
            browse: None,
        };
        assert!(
            matches!(read(&request, &cancel), Ok(Outcome::Page(page)) if page.directory == request.path)
        );
    }

    /// A newer request replaces queued I/O and revokes a reply even inside a blocked read.
    #[test]
    fn blocked_listing_cancellation_and_replacement() {
        let (started, reached) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let picker = Picker::with_reader(move |request, cancel| {
            started.send(request.generation).unwrap();
            if request.generation == 1 {
                gate.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(cancel.is_cancelled());
            }
            Ok(Outcome::File(request.path.clone()))
        })
        .unwrap();
        let request = |generation| Request {
            generation,
            path: PathBuf::from(format!("request-{generation}")),
            browse: Some(Browse::First),
        };
        picker.request(request(1));
        assert_eq!(reached.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        picker.request(request(2));
        picker.cancel();
        picker.request(request(3));
        release.send(()).unwrap();
        assert_eq!(reached.recv_timeout(Duration::from_secs(5)).unwrap(), 3);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(reply) = picker.poll() {
                assert_eq!(reply.generation, 3);
                assert!(
                    matches!(reply.result, Ok(Outcome::File(path)) if path == Path::new("request-3"))
                );
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        picker.shutdown().unwrap().join().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_non_utf8_names_are_openable_from_listing() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        let name = OsString::from_vec(b"native-\xff".to_vec());
        let path = temp.path().join(&name);
        fs::write(&path, "native text").unwrap();
        let cancel = Cancellation::default();
        let listing = list(temp.path(), &Browse::First, &cancel).unwrap();
        assert_eq!(listing.entries[0].name, name);
        let request = Request {
            generation: 1,
            path: listing.directory.join(&listing.entries[0].name),
            browse: None,
        };
        assert!(matches!(read(&request, &cancel), Ok(Outcome::File(selected)) if selected == path));
    }
}
