//! Machine-local scratch checkpoints with atomic, bounded-memory catalog updates.
//!
//! All operations here are blocking and belong on the recovery worker. A live
//! owner holds a kernel lock; the catalog lock serializes claims and publication.
//! Immutable objects become reachable only after their bytes are synchronized.

use crate::document::{
    ByteOffset, Cancellation, FaultInjector, Format, IoPoint, Newline, Snapshot,
};
use nix::libc;
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

const MAGIC: &[u8; 8] = b"EDECAT01";
const RECORD_BYTES: usize = 160;
/// Maximum retained catalog rows for a single UI/listing request.
pub(crate) const PAGE_ENTRIES: usize = 128;

mod service;
pub(crate) use service::{Request, Service, Value};

/// Opaque persistent identity, unrelated to process-local document counters.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Id(u128);

impl Id {
    /// Allocate from the host's random source; exclusive creation handles collisions.
    pub(crate) fn fresh() -> io::Result<Self> {
        let mut bytes = [0; 16];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let id = Self(u128::from_le_bytes(bytes));
        if id.0 == 0 { Self::fresh() } else { Ok(id) }
    }
}

/// Checkpointed view metadata always belongs to the captured content snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct View {
    pub(crate) caret: u64,
    pub(crate) anchor: u64,
    pub(crate) top: u64,
    pub(crate) left: u64,
    pub(crate) preferred: Option<u64>,
    pub(crate) order: u64,
    pub(crate) active: bool,
}

/// One fixed-width catalog entry; retired entries revoke late publication authority.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    pub(crate) id: Id,
    owner: Id,
    pub(crate) object: Id,
    creator: Id,
    group: u64,
    pub(crate) len: u64,
    pub(crate) format: Format,
    pub(crate) view: View,
    retired: bool,
    /// Bounded UTF-8 restore-error summary, NUL padded in the fixed catalog record.
    pub(crate) restore_error: [u8; 28],
}

/// Recovery policy independent of process-global environment during worker operation.
#[derive(Clone)]
pub(crate) struct Options {
    pub(crate) root: PathBuf,
    pub(crate) interval: Duration,
    pub(crate) reserve_bytes: u64,
    pub(crate) faults: Option<Arc<dyn FaultInjector>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            interval: Duration::from_secs(5),
            reserve_bytes: 512 << 20,
            faults: None,
        }
    }
}

impl Options {
    /// Resolve a profile once; a missing persistent location is never replaced by /tmp.
    pub(crate) fn environment() -> io::Result<Self> {
        let root = if let Some(root) = env::var_os("EDE_STATE_DIR") {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err(invalid("EDE_STATE_DIR must be an absolute path"));
            }
            root
        } else {
            let home = || {
                env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .ok_or_else(|| invalid("An absolute HOME or EDE_STATE_DIR is required"))
            };
            #[cfg(target_os = "macos")]
            {
                home()?.join("Library/Application Support/ede")
            }
            #[cfg(target_os = "linux")]
            {
                match env::var_os("XDG_STATE_HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                {
                    Some(path) => path.join("ede"),
                    None => home()?.join(".local/state/ede"),
                }
            }
        };
        Ok(Self {
            root,
            ..Self::default()
        })
    }
}

/// Blocking store owned by one recovery worker. Its owner lock survives every job.
pub(crate) struct Store {
    options: Options,
    owner: Id,
    owner_lock: File,
    registry: File,
    maintenance: Mutex<Option<String>>,
}

impl Store {
    /// Validate the state root and atomically claim scratches of every retired owner.
    pub(crate) fn open(options: Options) -> io::Result<Self> {
        if !options.root.is_absolute() || options.interval.is_zero() {
            return Err(invalid(
                "Recovery requires an absolute root and positive interval",
            ));
        }
        private_directory(&options.root)?;
        private_directory(&options.root.join("owners"))?;
        private_directory(&options.root.join("objects"))?;
        let registry = private_open(&options.root.join("registry.lock"), true)?;
        let bootstrap = lock_file(&registry, &Cancellation::default())?;
        let owner = Id::fresh()?;
        let owner_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(
                options
                    .root
                    .join("owners")
                    .join(format!("{:032x}", owner.0)),
            )?;
        if !try_lock(&owner_lock)? {
            return Err(invalid("New recovery owner lock was already held"));
        }
        drop(bootstrap);
        let store = Self {
            options,
            owner,
            owner_lock,
            registry,
            maintenance: Mutex::new(None),
        };
        let cancel = Cancellation::default();
        let _guard = store.lock(&cancel)?;
        let catalog_path = store.options.root.join("catalog");
        if !catalog_path.try_exists()? {
            store.rewrite(0, std::iter::empty())?;
        }
        // Fully validate the catalog before publishing any ownership change.
        let generation = store.reader()?.generation;
        for entry in store.reader()? {
            entry?;
        }
        let next = generation
            .checked_add(1)
            .ok_or_else(|| invalid("Recovery generation overflow"))?;
        let mut rows = tempfile::tempfile_in(&store.options.root)?;
        let mut owners = tempfile::tempfile_in(&store.options.root)?;
        let mut writer = BufWriter::with_capacity(64 << 10, &mut rows);
        for entry in store.reader()? {
            let entry = entry?;
            if store.classified_owner(&mut owners, entry.owner)? {
                write_entry(&mut writer, entry)?;
            }
        }
        store.check(IoPoint::Metadata)?;
        // Select groups in bounded memory. Startup may perform several scans,
        // but never retains metadata proportional to all abandoned instances.
        let mut ceiling = None;
        let mut order = 0u64;
        let mut first_group = true;
        loop {
            let mut selected = None;
            for entry in store.reader()? {
                let entry = entry?;
                let key = (entry.group, entry.owner);
                if !entry.retired
                    && !store.classified_owner(&mut owners, entry.owner)?
                    && ceiling.is_none_or(|ceiling| key < ceiling)
                    && selected.is_none_or(|selected| key > selected)
                {
                    selected = Some(key);
                }
            }
            let Some(group) = selected else {
                break;
            };
            let mut after = None;
            loop {
                let page = store.select_page(
                    |entry| (entry.group, entry.owner) == group && !entry.retired,
                    after,
                )?;
                if page.is_empty() {
                    break;
                }
                for mut entry in page {
                    after = Some((entry.view.order, entry.id));
                    entry.owner = owner;
                    entry.group = next;
                    entry.view.order = order;
                    entry.view.active &= first_group;
                    order = order
                        .checked_add(1)
                        .ok_or_else(|| invalid("Recovery order overflow"))?;
                    write_entry(&mut writer, entry)?;
                }
            }
            first_group = false;
            ceiling = Some(group);
        }
        writer.flush()?;
        drop(writer);
        rows.rewind()?;
        store.rewrite(next, RawEntries(BufReader::new(rows)))?;
        store.maintain();
        drop(_guard);
        Ok(store)
    }

    /// List one bounded page of this owner's retained entries, ordered by saved tab rank.
    pub(crate) fn page(&self, after: Option<(u64, Id)>) -> io::Result<Vec<Entry>> {
        self.select_page(|entry| entry.owner == self.owner && !entry.retired, after)
    }

    /// Page backwards without retaining a navigation stack proportional to the catalog.
    pub(crate) fn previous_page(&self, before: Option<(u64, Id)>) -> io::Result<Vec<Entry>> {
        let mut page: Vec<Entry> = Vec::with_capacity(PAGE_ENTRIES);
        for entry in self.reader()? {
            let entry = entry?;
            let key = (entry.view.order, entry.id);
            if entry.owner != self.owner
                || entry.retired
                || before.is_some_and(|before| key >= before)
            {
                continue;
            }
            let mut index =
                page.partition_point(|previous| (previous.view.order, previous.id) < key);
            if page.len() == PAGE_ENTRIES {
                if index == 0 {
                    continue;
                }
                page.remove(0);
                index -= 1;
            }
            page.insert(index, entry);
        }
        Ok(page)
    }

    /// Preserve creation order even when most claimed scratches remain unloaded.
    pub(crate) fn next_order(&self) -> io::Result<u64> {
        let mut next = 0;
        for entry in self.reader()? {
            let entry = entry?;
            if entry.owner == self.owner && !entry.retired {
                next = next.max(
                    entry
                        .view
                        .order
                        .checked_add(1)
                        .ok_or_else(|| invalid("Recovery order overflow"))?,
                );
            }
        }
        Ok(next)
    }

    /// Production scheduling interval resolved before the worker starts.
    pub(crate) fn interval(&self) -> Duration {
        self.options.interval
    }

    fn select_page(
        &self,
        predicate: impl Fn(&Entry) -> bool,
        after: Option<(u64, Id)>,
    ) -> io::Result<Vec<Entry>> {
        let mut page = Vec::with_capacity(PAGE_ENTRIES);
        for entry in self.reader()? {
            let entry = entry?;
            let key = (entry.view.order, entry.id);
            if !predicate(&entry) || after.is_some_and(|after| key <= after) {
                continue;
            }
            let index =
                page.partition_point(|previous: &Entry| (previous.view.order, previous.id) < key);
            if index < PAGE_ENTRIES {
                if page.len() == PAGE_ENTRIES {
                    page.pop();
                }
                page.insert(index, entry);
            }
        }
        Ok(page)
    }

    /// Return the immutable object path without admitting it as a named-file destination.
    pub(crate) fn object_path(&self, entry: &Entry) -> PathBuf {
        self.options
            .root
            .join("objects")
            .join(format!("{:032x}-{:032x}", entry.creator.0, entry.object.0))
    }

    /// Publish exact captured contents, or only update metadata when bytes are unchanged.
    pub(crate) fn checkpoint(
        &mut self,
        id: Id,
        snapshot: Option<Snapshot>,
        view: View,
        cancel: &Cancellation,
    ) -> io::Result<Entry> {
        check_cancel(cancel)?;
        let object = if let Some(snapshot) = snapshot {
            let object = Id::fresh()?;
            let mut output = tempfile::Builder::new()
                .prefix(&format!("{:032x}-", self.owner.0))
                .tempfile_in(self.options.root.join("objects"))?;
            self.reserve(output.as_file(), snapshot.len().saturating_add(3))?;
            if snapshot.format().bom {
                output.write_all(&[0xef, 0xbb, 0xbf])?;
            }
            let mut offset = 0;
            while offset < snapshot.len() {
                check_cancel(cancel)?;
                self.check(IoPoint::Write)?;
                let window = snapshot
                    .read_window_with_cancel(ByteOffset(offset), 64 << 10, cancel)
                    .map_err(document_error)?;
                output.write_all(window.text().as_bytes())?;
                offset += window.text().len() as u64;
            }
            self.check(IoPoint::Sync)?;
            output.as_file().sync_all()?;
            let entry = Entry {
                id,
                owner: self.owner,
                creator: self.owner,
                object,
                group: 0,
                len: snapshot.len(),
                format: snapshot.format(),
                view,
                retired: false,
                restore_error: [0; 28],
            };
            output
                .persist_noclobber(self.object_path(&entry))
                .map_err(|error| error.error)?;
            File::open(self.options.root.join("objects"))?.sync_all()?;
            Some(entry)
        } else {
            None
        };
        let _guard = self.lock(cancel)?;
        let mut replacement = object;
        for entry in self.reader()? {
            let entry = entry?;
            if entry.id == id {
                if entry.owner != self.owner || entry.retired {
                    return Err(invalid("Scratch checkpoint authority was revoked"));
                }
                if replacement.is_none() {
                    replacement = Some(Entry {
                        view,
                        restore_error: [0; 28],
                        ..entry
                    });
                }
            }
        }
        let mut replacement =
            replacement.ok_or_else(|| invalid("Scratch has no checkpointed contents"))?;
        let generation = self.next_generation()?;
        replacement.group = generation;
        let mut found = false;
        let mut rows = tempfile::tempfile_in(&self.options.root)?;
        {
            let mut writer = BufWriter::with_capacity(64 << 10, &mut rows);
            for entry in self.reader()? {
                let mut entry = entry?;
                if entry.id == id {
                    entry = replacement;
                    found = true;
                } else if entry.owner == self.owner {
                    entry.group = generation;
                    if view.active {
                        entry.view.active = false;
                    }
                }
                write_entry(&mut writer, entry)?;
            }
            if !found {
                write_entry(&mut writer, replacement)?;
            }
            writer.flush()?;
        }
        rows.rewind()?;
        check_cancel(cancel)?;
        self.rewrite(generation, RawEntries(BufReader::new(rows)))?;
        self.maintain();
        Ok(replacement)
    }

    /// Retain a bounded error with its object reference without changing group or order.
    pub(crate) fn record_error(
        &mut self,
        id: Id,
        error: [u8; 28],
        cancel: &Cancellation,
    ) -> io::Result<()> {
        let _guard = self.lock(cancel)?;
        let generation = self.next_generation()?;
        self.rewrite(
            generation,
            self.reader()?.map(|entry| {
                let mut entry = entry?;
                if entry.id == id && entry.owner == self.owner && !entry.retired {
                    entry.restore_error = error;
                }
                Ok(entry)
            }),
        )
    }

    /// Persist a tombstone before acknowledging discard, so late checkpoints cannot revive it.
    pub(crate) fn discard(&mut self, id: Id, cancel: &Cancellation) -> io::Result<()> {
        let _guard = self.lock(cancel)?;
        let generation = self.next_generation()?;
        let mut found = false;
        let mut rows = tempfile::tempfile_in(&self.options.root)?;
        {
            let mut writer = BufWriter::new(&mut rows);
            for entry in self.reader()? {
                let mut entry = entry?;
                if entry.id == id {
                    if entry.owner != self.owner {
                        return Err(invalid("Scratch belongs to another owner"));
                    }
                    entry.retired = true;
                    entry.group = generation;
                    found = true;
                }
                write_entry(&mut writer, entry)?;
            }
            if !found {
                write_entry(
                    &mut writer,
                    Entry {
                        id,
                        owner: self.owner,
                        creator: self.owner,
                        object: Id::default(),
                        group: generation,
                        len: 0,
                        format: Format::default(),
                        view: View::default(),
                        retired: true,
                        restore_error: [0; 28],
                    },
                )?;
            }
            writer.flush()?;
        }
        rows.rewind()?;
        if let Err(error) = self.rewrite(generation, RawEntries(BufReader::new(rows))) {
            let published = self
                .reader()
                .and_then(|mut entries| {
                    entries.try_fold(false, |found, entry| {
                        let entry = entry?;
                        Ok(found || entry.id == id && entry.owner == self.owner && entry.retired)
                    })
                })
                .unwrap_or(true);
            return Err(io::Error::other(RemovalFailure {
                source: error,
                published,
            }));
        }
        self.maintain();
        Ok(())
    }

    fn reader(&self) -> io::Result<Catalog> {
        Catalog::open(&self.options.root.join("catalog"))
    }

    fn next_generation(&self) -> io::Result<u64> {
        self.reader()?
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("Recovery generation overflow"))
    }

    fn lock(&self, cancel: &Cancellation) -> io::Result<Lock<'_>> {
        lock_file(&self.registry, cancel)
    }

    fn owner_live(&self, owner: Id) -> io::Result<bool> {
        if owner == self.owner {
            return Ok(true);
        }
        let file = match private_open(
            &self
                .options
                .root
                .join("owners")
                .join(format!("{:032x}", owner.0)),
            false,
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        // Release a successful probe even if a concurrent fork inherited the descriptor.
        let acquired = try_lock(&file)?;
        if acquired {
            unlock(&file);
        }
        Ok(!acquired)
    }

    /// Freeze liveness once per startup claim, even if an owner exits during a scan.
    /// The temporary disk table bounds memory when many retired owners are present.
    fn classified_owner(&self, owners: &mut File, owner: Id) -> io::Result<bool> {
        owners.rewind()?;
        let length = owners.metadata()?.len();
        let mut row = [0; 17];
        for _ in 0..length / 17 {
            owners.read_exact(&mut row)?;
            if row[..16] == owner.0.to_le_bytes() {
                return Ok(row[16] != 0);
            }
        }
        let live = self.owner_live(owner)?;
        row[..16].copy_from_slice(&owner.0.to_le_bytes());
        row[16] = u8::from(live);
        owners.write_all(&row)?;
        Ok(live)
    }

    fn rewrite(
        &self,
        generation: u64,
        entries: impl IntoIterator<Item = io::Result<Entry>>,
    ) -> io::Result<()> {
        let mut staging = tempfile::Builder::new()
            .prefix(&format!("catalog-stage-{:032x}-", self.owner.0))
            .tempfile_in(&self.options.root)?;
        let mut count = 0u64;
        {
            let mut writer = BufWriter::with_capacity(64 << 10, staging.as_file_mut());
            writer.write_all(MAGIC)?;
            writer.write_all(&generation.to_le_bytes())?;
            writer.write_all(&0u64.to_le_bytes())?;
            for entry in entries {
                self.reserve(writer.get_ref(), RECORD_BYTES as u64 + 4)?;
                write_entry(&mut writer, entry?)?;
                count = count
                    .checked_add(1)
                    .ok_or_else(|| invalid("Recovery catalog overflow"))?;
            }
            writer.flush()?;
        }
        staging.as_file_mut().seek(SeekFrom::Start(16))?;
        staging.write_all(&count.to_le_bytes())?;
        staging.as_file().sync_all()?;
        self.check(IoPoint::Publish)?;
        staging
            .persist(self.options.root.join("catalog"))
            .map_err(|error| error.error)?;
        self.check(IoPoint::DirectorySync)?;
        File::open(&self.options.root)?.sync_all()
    }

    fn collect_orphans(&self) -> io::Result<()> {
        // Owner creation and locking use the registry lock, so cleanup cannot
        // unlink a newly created owner inode before that process acquires it.
        for owner in fs::read_dir(self.options.root.join("owners"))? {
            let owner = owner?;
            let name = owner.file_name();
            let Some(name) = name.to_str().filter(|name| name.len() == 32) else {
                continue;
            };
            let Ok(id) = u128::from_str_radix(name, 16) else {
                continue;
            };
            if self.owner_live(Id(id))? {
                continue;
            }
            let mut referenced = false;
            for entry in self.reader()? {
                if entry?.owner == Id(id) {
                    referenced = true;
                    break;
                }
            }
            if !referenced {
                fs::remove_file(owner.path())?;
            }
        }
        for stage in fs::read_dir(&self.options.root)? {
            let stage = stage?;
            let name = stage.file_name();
            let Some(name) = name
                .to_str()
                .and_then(|name| name.strip_prefix("catalog-stage-"))
            else {
                continue;
            };
            let Some((owner, _)) = name.split_once('-') else {
                continue;
            };
            let Ok(owner) = u128::from_str_radix(owner, 16) else {
                continue;
            };
            if !self.owner_live(Id(owner))? {
                fs::remove_file(stage.path())?;
            }
        }
        for path in fs::read_dir(self.options.root.join("objects"))? {
            let path = path?;
            let name = path.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some((creator, _)) = name.split_once('-') else {
                continue;
            };
            let Ok(creator) = u128::from_str_radix(creator, 16) else {
                continue;
            };
            if Id(creator) != self.owner && self.owner_live(Id(creator))? {
                continue;
            }
            let mut referenced = false;
            for entry in self.reader()? {
                let entry = entry?;
                if !entry.retired
                    && self.object_path(&entry).file_name() == Some(std::ffi::OsStr::new(name))
                {
                    referenced = true;
                    break;
                }
            }
            if !referenced {
                fs::remove_file(path.path())?;
            }
        }
        Ok(())
    }

    /// Cleanup cannot invalidate an already acknowledged content/catalog publication.
    fn maintain(&self) {
        if let Err(error) = self.collect_orphans()
            && let Ok(mut warning) = self.maintenance.lock()
        {
            *warning = Some(
                format!("Recovery cleanup pending: {error}")
                    .chars()
                    .take(1024)
                    .collect(),
            );
        }
    }

    /// Surface bounded maintenance feedback separately from a committed operation's result.
    pub(crate) fn take_warning(&self) -> Option<String> {
        self.maintenance.lock().ok()?.take()
    }

    fn check(&self, point: IoPoint) -> io::Result<()> {
        self.options
            .faults
            .as_ref()
            .map_or(Ok(()), |faults| faults.check(point))
    }

    fn reserve(&self, file: &File, bytes: u64) -> io::Result<()> {
        let stat = nix::sys::statvfs::fstatvfs(file).map_err(io::Error::from)?;
        #[cfg(target_os = "macos")]
        let blocks = u64::from(stat.blocks_available());
        #[cfg(target_os = "linux")]
        let blocks = stat.blocks_available();
        if blocks.saturating_mul(stat.fragment_size())
            < bytes.saturating_add(self.options.reserve_bytes)
        {
            Err(io::Error::other("Insufficient recovery disk space"))
        } else {
            Ok(())
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Closing this descriptor alone leaves the lock held if a fork has yet to exec.
        unlock(&self.owner_lock);
    }
}

/// File-descriptor-scoped registry lock; never unlink the lock inode.
struct Lock<'a>(&'a File);
fn lock_file<'a>(file: &'a File, cancel: &Cancellation) -> io::Result<Lock<'a>> {
    loop {
        check_cancel(cancel)?;
        if try_lock(file)? {
            return Ok(Lock(file));
        }
        thread::sleep(Duration::from_millis(5));
    }
}
impl Drop for Lock<'_> {
    fn drop(&mut self) {
        unlock(self.0);
    }
}

/// Release the lock on the open file description, including any inherited copies.
fn unlock(file: &File) {
    // SAFETY: flock accepts a live descriptor and no pointers.
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

fn try_lock(file: &File) -> io::Result<bool> {
    // SAFETY: flock accepts a live descriptor and no pointers.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

fn private_directory(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Recovery directory {} must be owned by this user with mode 0700",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn private_open(path: &Path, create: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(invalid("Recovery metadata must be a private regular file"));
    }
    Ok(file)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn document_error(error: crate::document::Error) -> io::Error {
    if matches!(error, crate::document::Error::Cancelled) {
        io::Error::new(io::ErrorKind::Interrupted, error)
    } else {
        io::Error::other(error)
    }
}

/// An unsuccessful removal may nevertheless have revoked its old scratch identity.
#[derive(Debug)]
struct RemovalFailure {
    source: io::Error,
    published: bool,
}
impl std::fmt::Display for RemovalFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}
impl std::error::Error for RemovalFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Keep editable contents under a fresh identity if a failed removal published its tombstone.
pub(crate) fn removal_published(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|error| error.downcast_ref::<RemovalFailure>())
        .is_some_and(|failure| failure.published)
}

fn check_cancel(cancel: &Cancellation) -> io::Result<()> {
    if cancel.is_cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Recovery operation cancelled",
        ))
    } else {
        Ok(())
    }
}

/// Streaming catalog reader validates declared record count and trailing bytes.
struct Catalog {
    reader: BufReader<File>,
    generation: u64,
    remaining: u64,
    checked_end: bool,
}
impl Catalog {
    fn open(path: &Path) -> io::Result<Self> {
        let mut reader = BufReader::with_capacity(64 << 10, private_open(path, false)?);
        let mut header = [0; 24];
        reader.read_exact(&mut header)?;
        if &header[..8] != MAGIC {
            return Err(invalid(
                "Unsupported or corrupt recovery catalog; existing data retained",
            ));
        }
        let generation = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let remaining = u64::from_le_bytes(header[16..24].try_into().unwrap());
        let expected = remaining
            .checked_mul(RECORD_BYTES as u64 + 4)
            .and_then(|bytes| bytes.checked_add(24));
        if expected != Some(reader.get_ref().metadata()?.len()) {
            return Err(invalid(
                "Incomplete recovery catalog; existing data retained",
            ));
        }
        Ok(Self {
            reader,
            generation,
            remaining,
            checked_end: false,
        })
    }
}
impl Iterator for Catalog {
    type Item = io::Result<Entry>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            if self.checked_end {
                return None;
            }
            self.checked_end = true;
            return match self.reader.read(&mut [0]) {
                Ok(0) => None,
                Ok(_) => Some(Err(invalid("Trailing recovery metadata"))),
                Err(error) => Some(Err(error)),
            };
        }
        self.remaining -= 1;
        Some(read_entry(&mut self.reader))
    }
}

struct RawEntries(BufReader<File>);
impl Iterator for RawEntries {
    type Item = io::Result<Entry>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.0.fill_buf() {
            Ok([]) => None,
            Ok(_) => Some(read_entry(&mut self.0)),
            Err(error) => Some(Err(error)),
        }
    }
}

fn write_entry(writer: &mut impl Write, entry: Entry) -> io::Result<()> {
    let mut bytes = [0; RECORD_BYTES];
    for (offset, id) in [
        (0, entry.id),
        (16, entry.owner),
        (32, entry.object),
        (48, entry.creator),
    ] {
        bytes[offset..offset + 16].copy_from_slice(&id.0.to_le_bytes());
    }
    for (offset, value) in [
        (64, entry.group),
        (72, entry.view.order),
        (80, entry.len),
        (88, entry.view.caret),
        (96, entry.view.anchor),
        (104, entry.view.top),
        (112, entry.view.left),
        (120, entry.view.preferred.unwrap_or(u64::MAX)),
    ] {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    bytes[128] = u8::from(entry.view.active);
    bytes[129] = u8::from(entry.format.bom);
    bytes[130] = u8::from(entry.format.newline == Newline::CrLf);
    bytes[131] = u8::from(entry.retired);
    bytes[132..].copy_from_slice(&entry.restore_error);
    writer.write_all(&(RECORD_BYTES as u32).to_le_bytes())?;
    writer.write_all(&bytes)
}

fn read_entry(reader: &mut impl Read) -> io::Result<Entry> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    if u32::from_le_bytes(size) != RECORD_BYTES as u32 {
        return Err(invalid("Unsupported recovery record"));
    }
    let mut bytes = [0; RECORD_BYTES];
    reader.read_exact(&mut bytes)?;
    if bytes[128..132].iter().any(|value| *value > 1) || std::str::from_utf8(&bytes[132..]).is_err()
    {
        return Err(invalid("Invalid recovery metadata"));
    }
    let id = |offset| {
        Id(u128::from_le_bytes(
            bytes[offset..offset + 16].try_into().unwrap(),
        ))
    };
    let word = |offset| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
    if id(0).0 == 0 || id(16).0 == 0 {
        return Err(invalid("Invalid recovery identity"));
    }
    Ok(Entry {
        id: id(0),
        owner: id(16),
        object: id(32),
        creator: id(48),
        group: word(64),
        len: word(80),
        format: Format {
            bom: bytes[129] != 0,
            newline: if bytes[130] == 0 {
                Newline::Lf
            } else {
                Newline::CrLf
            },
        },
        view: View {
            order: word(72),
            caret: word(88),
            anchor: word(96),
            top: word(104),
            left: word(112),
            preferred: (word(120) != u64::MAX).then(|| word(120)),
            active: bytes[128] != 0,
        },
        retired: bytes[131] != 0,
        restore_error: bytes[132..].try_into().unwrap(),
    })
}

#[cfg(test)]
mod tests {
    //! Recovery acceptance checks use private profiles and real filesystem publication.

    use super::*;
    use crate::document::{Document, Edit, OpenOptions, TextRange};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    /// Stop at a real publication boundary once, with explicit cross-thread ordering.
    #[derive(Debug)]
    struct Gate {
        point: IoPoint,
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        armed: AtomicBool,
    }

    impl FaultInjector for Gate {
        fn check(&self, point: IoPoint) -> io::Result<()> {
            if point == self.point && self.armed.swap(false, Ordering::AcqRel) {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            Ok(())
        }
    }

    /// Reject one specific I/O boundary to distinguish pre- and post-publication failure.
    #[derive(Debug)]
    struct Fail(IoPoint);
    impl FaultInjector for Fail {
        fn check(&self, point: IoPoint) -> io::Result<()> {
            if point == self.0 {
                Err(io::Error::other("injected recovery failure"))
            } else {
                Ok(())
            }
        }
    }

    /// Construct a private store without imposing production disk headroom on tiny fixtures.
    fn open(root: &Path) -> Store {
        Store::open(Options {
            root: root.join("state"),
            reserve_bytes: 0,
            ..Options::default()
        })
        .unwrap()
    }

    /// Prepare real immutable document text rather than a mock persistence payload.
    fn document(text: &str) -> Document {
        let mut document = Document::new(OpenOptions::default()).unwrap();
        document
            .apply(
                document.revision(),
                [Edit {
                    range: TextRange::new(0, 0).unwrap(),
                    text,
                }],
                &Cancellation::default(),
            )
            .unwrap();
        document
    }

    /// R01/R04: active owners stay isolated and a later owner claims every retired set.
    #[test]
    fn independent_live_owners_and_combined_retired_recovery() {
        let root = tempfile::tempdir().unwrap();
        let mut a = open(root.path());
        let first = Id::fresh().unwrap();
        let text = document("first 界e\u{301}\r\nlast");
        a.checkpoint(
            first,
            Some(text.snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
        let mut b = open(root.path());
        assert!(b.page(None).unwrap().is_empty());
        let second = Id::fresh().unwrap();
        b.checkpoint(
            second,
            Some(document("second").snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
        drop(a);
        let c = open(root.path());
        assert_eq!(
            c.page(None)
                .unwrap()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            [first]
        );
        assert_eq!(
            b.page(None)
                .unwrap()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            [second]
        );
        drop(b);
        drop(c);
        let restored = open(root.path());
        let entries = restored.page(None).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| entry.id == first));
        assert!(entries.iter().any(|entry| entry.id == second));
        let first = entries.iter().find(|entry| entry.id == first).unwrap();
        assert_eq!(
            fs::read(restored.object_path(first)).unwrap(),
            "first 界e\u{301}\r\nlast".as_bytes()
        );
    }

    /// R04: simultaneous startup claims partition one retired catalog without duplicates.
    #[test]
    fn simultaneous_claims_never_duplicate_or_lose_scratches() {
        let root = tempfile::tempdir().unwrap();
        let mut previous = open(root.path());
        let mut expected = Vec::new();
        for order in 0..8 {
            let id = Id::fresh().unwrap();
            previous
                .checkpoint(
                    id,
                    Some(document("retained").snapshot().unwrap()),
                    View {
                        order,
                        ..View::default()
                    },
                    &Cancellation::default(),
                )
                .unwrap();
            expected.push(id);
        }
        drop(previous);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let root = root.path().to_owned();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    open(&root)
                })
            })
            .collect();
        let owners: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let mut actual: Vec<_> = owners
            .iter()
            .flat_map(|store| store.page(None).unwrap().into_iter().map(|entry| entry.id))
            .collect();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
    }

    /// R06: a discarded identity cannot be reinserted by a late checkpoint.
    #[test]
    fn discard_revokes_late_checkpoint_and_keeps_other_scratch() {
        let root = tempfile::tempdir().unwrap();
        let mut store = open(root.path());
        let id = Id::fresh().unwrap();
        let text = document("must stay discarded");
        store
            .checkpoint(
                id,
                Some(text.snapshot().unwrap()),
                View::default(),
                &Cancellation::default(),
            )
            .unwrap();
        store.discard(id, &Cancellation::default()).unwrap();
        assert!(
            store
                .checkpoint(
                    id,
                    Some(text.snapshot().unwrap()),
                    View::default(),
                    &Cancellation::default()
                )
                .is_err()
        );
        assert!(store.page(None).unwrap().is_empty());
        drop(store);
        assert!(open(root.path()).page(None).unwrap().is_empty());
    }

    /// R02: metadata-only checkpoints reuse immutable text and retain the matching view.
    #[test]
    fn view_only_checkpoint_reuses_text_object() {
        let root = tempfile::tempdir().unwrap();
        let mut store = open(root.path());
        let id = Id::fresh().unwrap();
        let before = store
            .checkpoint(
                id,
                Some(document("abc").snapshot().unwrap()),
                View::default(),
                &Cancellation::default(),
            )
            .unwrap();
        let view = View {
            caret: 2,
            anchor: 1,
            top: 0,
            left: 9,
            preferred: Some(17),
            active: true,
            order: 4,
        };
        let after = store
            .checkpoint(id, None, view, &Cancellation::default())
            .unwrap();
        assert_eq!(before.object, after.object);
        assert_eq!(after.view, view);
        drop(store);
        let recovered = open(root.path());
        assert_eq!(recovered.page(None).unwrap()[0].view.caret, 2);
    }

    /// A malformed catalog is reported without replacing any existing bytes.
    #[test]
    fn malformed_catalog_is_never_replaced_with_empty_state() {
        let root = tempfile::tempdir().unwrap();
        drop(open(root.path()));
        let path = root.path().join("state/catalog");
        fs::write(&path, b"incomplete catalog").unwrap();
        assert!(
            Store::open(Options {
                root: root.path().join("state"),
                reserve_bytes: 0,
                ..Options::default()
            })
            .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), b"incomplete catalog");
    }

    /// R04: an owner exiting after live-copy classification cannot be copied again as retired.
    #[test]
    fn owner_exit_mid_claim_never_duplicates_scratch_identity() {
        let root = tempfile::tempdir().unwrap();
        let mut a = open(root.path());
        let id = Id::fresh().unwrap();
        a.checkpoint(
            id,
            Some(document("one copy only").snapshot().unwrap()),
            View::default(),
            &Cancellation::default(),
        )
        .unwrap();
        let (entered, observed) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let options = Options {
            root: root.path().join("state"),
            reserve_bytes: 0,
            faults: Some(Arc::new(Gate {
                point: IoPoint::Metadata,
                entered,
                release: Mutex::new(released),
                armed: AtomicBool::new(true),
            })),
            ..Options::default()
        };
        let claiming = thread::spawn(move || Store::open(options).unwrap());
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        // Model a child between fork and exec that still holds the close-on-exec descriptor.
        let inherited_owner_lock = a.owner_lock.try_clone().unwrap();
        drop(a);
        release.send(()).unwrap();
        let b = claiming.join().unwrap();
        assert!(b.page(None).unwrap().is_empty());
        drop(b);
        let c = open(root.path());
        let entries = c.page(None).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "the exiting owner's scratch must be recovered exactly once"
        );
        assert_eq!(entries[0].id, id);
        drop(inherited_owner_lock);
    }

    /// R03/R07: failed staging leaves the complete previous checkpoint and its text reachable.
    #[test]
    fn failed_checkpoint_preserves_previous_complete_publication() {
        for point in [IoPoint::Write, IoPoint::Sync, IoPoint::Publish] {
            let root = tempfile::tempdir().unwrap();
            let mut store = open(root.path());
            let id = Id::fresh().unwrap();
            let first = store
                .checkpoint(
                    id,
                    Some(document("checkpoint A").snapshot().unwrap()),
                    View::default(),
                    &Cancellation::default(),
                )
                .unwrap();
            store.options.faults = Some(Arc::new(Fail(point)));
            assert!(
                store
                    .checkpoint(
                        id,
                        Some(document("checkpoint B").snapshot().unwrap()),
                        View::default(),
                        &Cancellation::default()
                    )
                    .is_err()
            );
            let retained = store.page(None).unwrap();
            assert_eq!(retained[0].object, first.object);
            assert_eq!(
                fs::read(store.object_path(&retained[0])).unwrap(),
                b"checkpoint A"
            );
            drop(store);
            let restored = open(root.path());
            assert_eq!(
                fs::read(restored.object_path(&restored.page(None).unwrap()[0])).unwrap(),
                b"checkpoint A"
            );
        }
    }

    /// A failed sync after tombstone publication explicitly reports revoked old authority.
    #[test]
    fn uncertain_discard_reports_publication_and_new_identity_can_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let mut store = open(root.path());
        let id = Id::fresh().unwrap();
        let text = document("still editable after failure");
        store
            .checkpoint(
                id,
                Some(text.snapshot().unwrap()),
                View::default(),
                &Cancellation::default(),
            )
            .unwrap();
        store.options.faults = Some(Arc::new(Fail(IoPoint::DirectorySync)));
        let error = store.discard(id, &Cancellation::default()).unwrap_err();
        assert!(removal_published(&error));
        store.options.faults = None;
        let replacement = Id::fresh().unwrap();
        store
            .checkpoint(
                replacement,
                Some(text.snapshot().unwrap()),
                View::default(),
                &Cancellation::default(),
            )
            .unwrap();
        drop(store);
        let restored = open(root.path());
        let entries = restored.page(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, replacement);
        assert_eq!(
            fs::read(restored.object_path(&entries[0])).unwrap(),
            b"still editable after failure"
        );
    }

    /// A test-process checkpoint pauses on a parent-observable marker until forcibly terminated.
    #[derive(Debug)]
    struct CrashGate {
        point: IoPoint,
        marker: PathBuf,
    }
    impl FaultInjector for CrashGate {
        fn check(&self, point: IoPoint) -> io::Result<()> {
            if point == self.point {
                fs::write(&self.marker, b"blocked at checkpoint boundary")?;
                loop {
                    thread::park();
                }
            }
            Ok(())
        }
    }

    /// Test-only child entry point; absent explicit isolated test variables it does no work.
    #[test]
    fn crash_checkpoint_child() {
        let Some(root) = std::env::var_os("EDE_RECOVERY_CRASH_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let mut store = open(&root);
        let id = Id::fresh().unwrap();
        store
            .checkpoint(
                id,
                Some(document("complete A").snapshot().unwrap()),
                View::default(),
                &Cancellation::default(),
            )
            .unwrap();
        let point = match std::env::var("EDE_RECOVERY_CRASH_POINT").unwrap().as_str() {
            "write" => IoPoint::Write,
            "sync" => IoPoint::Sync,
            "publish" => IoPoint::Publish,
            "directory" => IoPoint::DirectorySync,
            _ => panic!("invalid test-only crash point"),
        };
        store.options.faults = Some(Arc::new(CrashGate {
            point,
            marker: root.join("blocked"),
        }));
        store
            .checkpoint(
                id,
                Some(document("complete B").snapshot().unwrap()),
                View::default(),
                &Cancellation::default(),
            )
            .unwrap();
        panic!("parent must terminate the blocked checkpoint");
    }

    /// Always kill and reap a fixture if a parent-side crash assertion fails.
    struct CrashChild(std::process::Child);
    impl Drop for CrashChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// R03/R04: real process death releases ownership and never exposes partial checkpoint text.
    #[test]
    fn process_death_during_checkpoint_recovers_only_complete_publication() {
        for point in ["write", "sync", "publish", "directory"] {
            let root = tempfile::tempdir().unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "recovery::tests::crash_checkpoint_child",
                    "--nocapture",
                ])
                .env("EDE_RECOVERY_CRASH_ROOT", root.path())
                .env("EDE_RECOVERY_CRASH_POINT", point)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let mut child = CrashChild(child);
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !root.path().join("blocked").try_exists().unwrap() {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "checkpoint fixture stopped before the gate"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "checkpoint never reached {point}"
                );
                thread::sleep(Duration::from_millis(2));
            }
            assert!(child.0.try_wait().unwrap().is_none());
            child.0.kill().unwrap();
            assert!(!child.0.wait().unwrap().success());
            let recovered = open(root.path());
            let entries = recovered.page(None).unwrap();
            assert_eq!(entries.len(), 1);
            let expected = if point == "directory" {
                b"complete B"
            } else {
                b"complete A"
            };
            assert_eq!(
                fs::read(recovered.object_path(&entries[0])).unwrap(),
                expected
            );
        }
    }

    /// R05: pages can traverse a catalog larger than both a tab session and one listing page.
    #[test]
    fn recovery_pages_are_bounded_bidirectional_and_have_no_total_cutoff() {
        let root = tempfile::tempdir().unwrap();
        let mut store = open(root.path());
        let id = Id::fresh().unwrap();
        let entry = store
            .checkpoint(
                id,
                Some(document("fixture text").snapshot().unwrap()),
                View::default(),
                &Cancellation::default(),
            )
            .unwrap();
        // Reuse one immutable fixture object, isolating paging from content-copy cost.
        let fixtures = (0..131).map(|order| {
            Ok(Entry {
                id: Id::fresh().unwrap(),
                view: View {
                    order,
                    ..View::default()
                },
                ..entry
            })
        });
        store
            .rewrite(store.next_generation().unwrap(), fixtures)
            .unwrap();
        drop(store);
        let restored = open(root.path());
        let first = restored.page(None).unwrap();
        assert_eq!(first.len(), PAGE_ENTRIES);
        let last = first.last().unwrap();
        let second = restored.page(Some((last.view.order, last.id))).unwrap();
        assert_eq!(second.len(), 3);
        let previous = restored
            .previous_page(Some((second[0].view.order, second[0].id)))
            .unwrap();
        assert_eq!(
            previous.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            first.iter().map(|entry| entry.id).collect::<Vec<_>>()
        );
        assert_eq!(second.last().unwrap().view.order, 130);
    }
}
