//! Parse an entire settings revision and publish edits using a verified disk base.

use super::{Context, Revision, Scalar, values::Values};
use nix::{
    errno::Errno,
    fcntl::{AtFlags, Flock, FlockArg, OFlag, open, openat, renameat},
    sys::stat::{Mode, fchmod, mkdirat},
    unistd::{UnlinkatFlags, linkat, unlinkat},
};
use std::{
    fs::{self, File, Metadata},
    hash::{BuildHasher, RandomState},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use toml::{Table, Value};
use toml_edit::{Array, DocumentMut, Item, TableLike};

const NAME: &str = "config.toml";
const STAGED_NAME: &str = "revision";
const LIMIT: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
}

impl From<&Metadata> for Identity {
    fn from(meta: &Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            mode: meta.mode(),
            links: meta.nlink(),
        }
    }
}

/// Disk observation captured by the last accepted revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Base {
    Missing,
    Present(Arc<Observation>),
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Observation {
    identity: Identity,
    bytes: String,
}

impl Base {
    pub fn present(&self) -> bool {
        matches!(self, Self::Present(_))
    }

    fn bytes(&self) -> &str {
        match self {
            Self::Missing => "",
            Self::Present(file) => &file.bytes,
        }
    }
}

/// A post-publication failure requires reloading before another save.
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub message: String,
    pub uncertain: bool,
}

fn invalid(reason: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.into())
}

/// Untrusted filenames, TOML keys and error details receive terminal escaping.
fn diagnostic(root: &Path, error: io::Error, uncertain: bool) -> Failure {
    let path = root.join(NAME);
    let detail = if uncertain {
        format!("file may have changed; reload settings: {error}")
    } else {
        error.to_string()
    };
    Failure {
        message: crate::text::safe(&format!("{path:?}: {detail}")),
        uncertain,
    }
}

fn parse(text: &str) -> io::Result<Revision> {
    let mut table = text.parse::<Table>().map_err(|error| {
        let (line, column) = error.span().map_or((1, 1), |span| {
            let prefix = &text.as_bytes()[..span.start.min(text.len())];
            let line = memchr::memchr_iter(b'\n', prefix).count() + 1;
            let column =
                memchr::memrchr(b'\n', prefix).map_or(prefix.len() + 1, |i| prefix.len() - i);
            (line, column)
        });
        invalid(format!("line {line}, column {column}: {}", error.message()))
    })?;
    let mut settings = Revision::default();
    if let Some(bindings) = table.remove("keybindings") {
        let Value::Table(bindings) = bindings else {
            return Err(invalid("keybindings: expected table"));
        };
        for (name, entries) in bindings {
            let context = Context::ALL
                .into_iter()
                .find(|context| context.id() == name)
                .ok_or_else(|| invalid(format!("keybindings.{name}: unknown context")))?;
            let Value::Table(entries) = entries else {
                return Err(invalid(format!("keybindings.{name}: expected table")));
            };
            for (id, shortcuts) in entries {
                let field = format!("keybindings.{name}.{id}");
                let Value::Array(shortcuts) = shortcuts else {
                    return Err(invalid(format!("{field}: expected shortcut array")));
                };
                if shortcuts.len() > 8 {
                    return Err(invalid(format!("{field}: at most eight shortcuts")));
                }
                let chords = shortcuts
                    .iter()
                    .map(|value| {
                        let chord = value
                            .as_str()
                            .ok_or_else(|| invalid(format!("{field}: expected shortcut string")))?;
                        chord
                            .parse()
                            .map_err(|reason| invalid(format!("{field}: {chord:?}: {reason}")))
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                settings
                    .map
                    .set_override(context, &id, chords)
                    .map_err(invalid)?;
            }
        }
    }
    settings.map.revalidate().map_err(invalid)?;
    if let Some(clipboard) = table.remove("clipboard") {
        let Value::Table(entries) = clipboard else {
            return Err(invalid("clipboard: expected table"));
        };
        for (key, value) in entries {
            if key != "copy_to_system" {
                return Err(invalid(format!("clipboard.{key}: unknown setting")));
            }
            settings.values.system_clipboard_override = Some(
                value
                    .as_bool()
                    .ok_or_else(|| invalid("clipboard.copy_to_system: expected boolean"))?,
            );
        }
    }
    for (name, entries) in table {
        if !matches!(name.as_str(), "recovery" | "highlighting") {
            return Err(invalid(format!("{name}: unknown setting")));
        }
        let Value::Table(entries) = entries else {
            return Err(invalid(format!("{name}: expected table")));
        };
        for (key, value) in entries {
            let setting = Scalar::ALL
                .into_iter()
                .find(|setting| setting.table() == name && setting.id() == key)
                .ok_or_else(|| invalid(format!("{name}.{key}: unknown setting")))?;
            let value = setting
                .validate(value.as_integer().and_then(|n| u64::try_from(n).ok()))
                .map_err(invalid)?;
            settings.values.set(setting, Some(value));
        }
    }
    Ok(settings)
}

fn directory(path: &Path) -> io::Result<File> {
    Ok(open(
        path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?
    .into())
}

fn open_named(directory: &File, name: &str) -> io::Result<Option<File>> {
    match openat(
        directory,
        name,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => Ok(Some(fd.into())),
        Err(Errno::ENOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn directory_at(parent: &File, name: &str) -> io::Result<File> {
    Ok(openat(
        parent,
        name,
        OFlag::O_RDONLY
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?
    .into())
}

/// Detect edits made to the same inode while a bounded observation was in progress.
fn unchanged(before: &Metadata, after: &Metadata) -> bool {
    Identity::from(before) == Identity::from(after)
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.len() == after.len()
}

fn observe(directory: &File, writing: bool) -> io::Result<Base> {
    observe_named(directory, NAME, writing)
}

fn observe_named(directory: &File, name: &str, writing: bool) -> io::Result<Base> {
    let Some(file) = open_named(directory, name)? else {
        return Ok(Base::Missing);
    };
    let attributes = file.metadata()?;
    if !attributes.is_file() {
        return Err(invalid("expected a regular file"));
    }
    if writing && attributes.nlink() != 1 {
        return Err(invalid("refusing to replace a multiply-linked file"));
    }
    let mut bytes = Vec::new();
    (&file).take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(invalid("file exceeds 64 KiB"));
    }
    let reopened = open_named(directory, name)?
        .ok_or_else(changed)?
        .metadata()?;
    if !unchanged(&attributes, &file.metadata()?) || !unchanged(&attributes, &reopened) {
        return Err(changed());
    }
    Ok(Base::Present(Arc::new(Observation {
        identity: Identity::from(&attributes),
        bytes: String::from_utf8(bytes).map_err(|_| invalid("file must be UTF-8"))?,
    })))
}

/// Read without taking the writer lock or creating a directory.
pub(crate) fn load(root: &Path) -> Result<(Revision, Base), Failure> {
    let read = || {
        let base = match directory(root) {
            Ok(directory) => observe(&directory, false)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Base::Missing,
            Err(error) => return Err(error),
        };
        Ok((parse(base.bytes())?, base))
    };
    read().map_err(|error| diagnostic(root, error, false))
}

fn table<'a>(parent: &'a mut dyn TableLike, name: &str) -> io::Result<&'a mut dyn TableLike> {
    if !parent.contains_key(name) {
        parent.insert(name, Item::Table(toml_edit::Table::new()));
    }
    parent
        .get_mut(name)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| invalid("expected editable TOML table"))
}

fn change(table: &mut dyn TableLike, key: &str, mut item: Item) {
    if let Some(old) = table.get_mut(key) {
        if let (Some(current), Some(replacement)) = (old.as_value(), item.as_value_mut()) {
            *replacement.decor_mut() = current.decor().clone();
        }
        *old = item;
    } else {
        table.insert(key, item);
    }
}

fn serialize(base: &Base, draft: &Revision) -> io::Result<String> {
    let previous = parse(base.bytes())?;
    let mut doc = base
        .bytes()
        .parse::<DocumentMut>()
        .map_err(|error| invalid(error.to_string()))?;
    let mut comments = String::new();
    edit_scalars(
        doc.as_table_mut(),
        previous.values,
        draft.values,
        &mut comments,
    )?;
    if previous.values.system_clipboard_override != draft.values.system_clipboard_override {
        let clipboard = table(doc.as_table_mut(), "clipboard")?;
        if let Some(value) = draft.values.system_clipboard_override {
            change(clipboard, "copy_to_system", toml_edit::value(value));
        } else {
            remove(clipboard, "copy_to_system", &mut comments);
        }
    }
    for (before, after) in previous.map.rows.iter().zip(&draft.map.rows) {
        if before.override_keys == after.override_keys {
            continue;
        }
        let bindings = table(doc.as_table_mut(), "keybindings")?;
        let context = table(bindings, after.context.id())?;
        let id = after.command.id();
        if let Some(chords) = &after.override_keys {
            let mut array = context
                .get(id)
                .and_then(Item::as_array)
                .cloned()
                .unwrap_or_default();
            edit_array(&mut array, chords, &mut comments);
            change(context, id, toml_edit::value(array));
        } else {
            remove(context, id, &mut comments);
        }
    }
    if !comments.is_empty() {
        doc.set_trailing(format!(
            "{}\n{comments}",
            doc.trailing().as_str().unwrap_or("")
        ));
    }
    let bytes = doc.to_string();
    if bytes.len() as u64 > LIMIT {
        return Err(invalid("serialized file exceeds 64 KiB"));
    }
    if parse(&bytes)? != *draft {
        return Err(invalid(
            "serialized file did not reproduce the validated settings",
        ));
    }
    Ok(bytes)
}

fn edit_scalars(
    doc: &mut dyn TableLike,
    previous: Values,
    draft: Values,
    comments: &mut String,
) -> io::Result<()> {
    for setting in Scalar::ALL {
        let updated = draft.override_value(setting);
        if previous.override_value(setting) != updated {
            let section = table(doc, setting.table())?;
            if let Some(value) = updated {
                change(section, setting.id(), toml_edit::value(value as i64));
            } else {
                remove(section, setting.id(), comments);
            }
        }
    }
    Ok(())
}

fn edit_array(array: &mut Array, chords: &[super::Chord], comments: &mut String) {
    for (index, chord) in chords.iter().enumerate() {
        let canonical = chord.to_string();
        if index >= array.len() {
            array.push(canonical);
        } else if array
            .get(index)
            .and_then(toml_edit::Value::as_str)
            .and_then(|old| old.parse().ok())
            != Some(*chord)
        {
            array.replace(index, canonical);
        }
    }
    while array.len() > chords.len() {
        collect_decor(array.remove(array.len() - 1).decor(), comments);
    }
}

/// Keep orphaned comments as standalone comments when an override is removed.
fn remove(table: &mut dyn TableLike, key: &str, comments: &mut String) {
    if let Some(key) = table.key(key) {
        collect_decor(key.leaf_decor(), comments);
        collect_decor(key.dotted_decor(), comments);
    }
    if let Some(item) = table.remove(key)
        && let Some(value) = item.as_value()
    {
        collect_decor(value.decor(), comments);
        if let Some(array) = value.as_array() {
            for item in array.iter() {
                collect_decor(item.decor(), comments);
            }
            collect_comments(array.trailing(), comments);
        }
    }
}

fn collect_decor(decor: &toml_edit::Decor, comments: &mut String) {
    for raw in [decor.prefix(), decor.suffix()].into_iter().flatten() {
        collect_comments(raw, comments);
    }
}

fn collect_comments(raw: &toml_edit::RawString, comments: &mut String) {
    for line in raw.as_str().unwrap_or("").lines() {
        if let Some(comment) = line.trim_start().strip_prefix('#') {
            comments.push('#');
            comments.push_str(comment);
            comments.push('\n');
        }
    }
}

struct Staged<'a> {
    parent: &'a File,
    directory: File,
    name: String,
    file: File,
}

impl<'a> Staged<'a> {
    fn create(parent: &'a File) -> io::Result<Self> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
            let nonce = RandomState::new().hash_one((std::process::id(), serial));
            let name = format!(".config-{nonce:016x}.tmp");
            match mkdirat(parent, name.as_str(), Mode::from_bits_truncate(0o700)) {
                Ok(()) => {}
                Err(Errno::EEXIST) => continue,
                Err(error) => return Err(error.into()),
            }
            let directory = directory_at(parent, &name)?;
            let metadata = directory.metadata()?;
            // SAFETY: geteuid has no preconditions and does not mutate process state.
            let uid = unsafe { nix::libc::geteuid() };
            if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "settings staging directory must be private",
                ));
            }
            return match openat(
                &directory,
                STAGED_NAME,
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            ) {
                Ok(file) => Ok(Self {
                    parent,
                    directory,
                    name,
                    file: file.into(),
                }),
                Err(error) => {
                    remove_stage_dir(parent, &name, &directory);
                    Err(error.into())
                }
            };
        }
        Err(io::Error::other("could not create a private settings file"))
    }
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        if let (Ok(Some(named)), Ok(staged)) = (
            open_named(&self.directory, STAGED_NAME),
            self.file.metadata(),
        ) && let Ok(named) = named.metadata()
            && same_file(&named, &staged)
        {
            let _ = unlinkat(&self.directory, STAGED_NAME, UnlinkatFlags::NoRemoveDir);
        }
        remove_stage_dir(self.parent, &self.name, &self.directory);
    }
}

fn remove_stage_dir(parent: &File, name: &str, directory: &File) {
    if let Ok(named) = directory_at(parent, name)
        && let (Ok(named), Ok(held)) = (named.metadata(), directory.metadata())
        && same_file(&named, &held)
    {
        let _ = unlinkat(parent, name, UnlinkatFlags::RemoveDir);
    }
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn changed() -> io::Error {
    io::Error::other("configuration changed on disk; reload settings before saving")
}

fn lock(directory: &File) -> io::Result<Flock<File>> {
    let file = File::from(openat(
        directory,
        ".config.lock",
        OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?);
    let attributes = file.metadata()?;
    if !attributes.is_file() || attributes.nlink() != 1 {
        return Err(invalid("invalid settings lock file"));
    }
    Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| {
        if error == Errno::EWOULDBLOCK {
            io::Error::other("Busy: another ede instance is saving settings")
        } else {
            error.into()
        }
    })
}

/// Compare, stage, grant, recheck and atomically publish one candidate.
pub(crate) fn save(
    root: &Path,
    base: &Base,
    draft: Revision,
    grant: impl FnOnce() -> bool,
) -> Result<(Revision, Base), Failure> {
    save_with(root, base, draft, grant, || {}, |dir| dir.sync_all())
}

fn save_with(
    root: &Path,
    base: &Base,
    draft: Revision,
    grant: impl FnOnce() -> bool,
    before_publication: impl FnOnce(),
    sync_publication: impl FnOnce(&File) -> io::Result<()>,
) -> Result<(Revision, Base), Failure> {
    let mut published = false;
    let write = || {
        // Validate before a write establishes any new directories or lock files.
        let bytes = serialize(base, &draft)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        let dir = directory(root)?;
        let _lock = lock(&dir)?;
        if observe(&dir, true)? != *base {
            return Err(changed());
        }
        let mut staged = Staged::create(&dir)?;
        staged.file.write_all(bytes.as_bytes())?;
        let mode = match base {
            Base::Missing => 0o600,
            Base::Present(file) => file.identity.mode & 0o7777,
        };
        fchmod(&staged.file, Mode::from_bits_truncate(mode as _))?;
        staged.file.sync_all()?;
        if !grant() {
            return Err(io::Error::other("Settings save cancelled"));
        }
        let staged_metadata = staged.file.metadata()?;
        if staged_metadata.mode() & 0o7777 != mode {
            return Err(io::Error::other(
                "could not preserve settings file permissions",
            ));
        }
        let replacement = Base::Present(Arc::new(Observation {
            identity: Identity::from(&staged_metadata),
            bytes,
        }));
        if observe_named(&staged.directory, STAGED_NAME, true)? != replacement {
            return Err(changed());
        }
        let held = dir.metadata()?;
        if observe(&dir, true)? != *base {
            return Err(changed());
        }
        before_publication();
        let source = open_named(&staged.directory, STAGED_NAME)?
            .ok_or_else(changed)?
            .metadata()?;
        if !unchanged(&staged_metadata, &source) {
            return Err(changed());
        }
        if !same_file(&directory(root)?.metadata()?, &held) {
            return Err(changed());
        }
        if *base == Base::Missing {
            if let Err(error) = linkat(&staged.directory, STAGED_NAME, &dir, NAME, AtFlags::empty())
            {
                return Err(if error == Errno::EEXIST {
                    changed()
                } else {
                    error.into()
                });
            }
        } else {
            renameat(&staged.directory, STAGED_NAME, &dir, NAME)?;
        }
        published = true;
        drop(staged);
        sync_publication(&dir)?;
        if observe(&dir, true)? != replacement {
            return Err(changed());
        }
        let selected = directory(root)?.metadata()?;
        if !same_file(&selected, &held) {
            return Err(changed());
        }
        Ok((draft, replacement))
    };
    write().map_err(|error| diagnostic(root, error, published))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::{PermissionsExt, symlink},
        sync::mpsc,
        time::Duration,
    };

    fn editing() -> Revision {
        parse("[recovery]\ncheckpoint_interval_seconds = 11\n").unwrap()
    }

    #[test]
    fn full_schema_rejects_invalid_values_and_resolves_complete_effective_bindings() {
        for scalar in Scalar::ALL {
            for valid in [1, scalar.maximum()] {
                let text = format!("[{}]\n{} = {valid}", scalar.table(), scalar.id());
                assert_eq!(parse(&text).unwrap().values.get(scalar), valid);
            }
            for bad in [
                "0".to_owned(),
                (scalar.maximum() + 1).to_string(),
                "-1".into(),
                "1.0".into(),
                "'1'".into(),
            ] {
                assert!(
                    parse(&format!("[{}]\n{} = {bad}", scalar.table(), scalar.id()))
                        .unwrap_err()
                        .to_string()
                        .contains(scalar.id())
                );
            }
        }
        for (text, error) in [
            ("[recovery\n", "line 1, column"),
            ("foo = 1", "unknown setting"),
            ("[keybindings.invalid]\nsave = ['F9']", "unknown context"),
            ("[keybindings.editor]\nsave = ['F9']", "unknown command"),
            ("[keybindings.global]\nsave = 3", "expected shortcut array"),
            (
                "[keybindings.global]\nsave = [9]",
                "expected shortcut string",
            ),
            (
                "[keybindings.global]\nsave = ['Shift+A']",
                "reserved for typing",
            ),
            (
                "[keybindings.global]\nsave = ['Cmd+S','Super+s']",
                "conflicts with save",
            ),
            (
                "[keybindings.global]\nsave = ['F6']",
                "conflicts with global.save",
            ),
            (
                "[keybindings.global]\nquit = ['Cmd+Q']",
                "Quit requires a portable shortcut",
            ),
            (
                "[keybindings.settings]\nactivate = []",
                "Activate requires a portable shortcut",
            ),
            (
                "[keybindings.search]\nfocus_editor = ['Ctrl+H']",
                "conflicts with global.replace",
            ),
            (
                "[keybindings.global]\nsave = ['F9','F9','F9','F9','F9','F9','F9','F9','F9']",
                "at most eight",
            ),
        ] {
            let diagnostic = parse(text).unwrap_err().to_string();
            assert!(diagnostic.contains(error), "{text:?}: {diagnostic:?}");
        }
        let valid = parse("[keybindings.global]\nreload_settings = []\nsave = ['F9']\n[keybindings.picker]\nactivate = ['Ctrl+O']").unwrap();
        assert_eq!(
            valid
                .map
                .shortcuts(Context::Global, crate::command::Command::ReloadSettings),
            ""
        );
        assert_eq!(
            valid
                .map
                .shortcuts(Context::Global, crate::command::Command::Save),
            "F9"
        );
        assert_eq!(
            valid
                .map
                .shortcuts(Context::Picker, crate::command::Command::Activate),
            "Ctrl+O"
        );
        assert!(parse("[keybindings.global]\nsave = ['F4']\nsave_as = ['Ctrl+S']").is_ok());
    }

    #[test]
    fn bounded_regular_no_follow_reads_and_safe_diagnostics() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("config-\x1b");
        let path = root.join(NAME);
        assert_eq!(load(&root).unwrap().1, Base::Missing);
        assert!(!root.exists());
        fs::create_dir(&root).unwrap();
        symlink(root.join("unavailable"), &path).unwrap();
        let diag = load(&root).unwrap_err().message;
        assert!(!diag.contains('\x1b'));
        assert!(diag.contains("config-\\u{1b}"));
        fs::remove_file(&path).unwrap();
        nix::unistd::mkfifo(&path, Mode::S_IRUSR).unwrap();
        assert!(load(&root).unwrap_err().message.contains("regular file"));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(load(&root).unwrap_err().message.contains("regular file"));
        fs::remove_dir(&path).unwrap();
        fs::write(&path, vec![b' '; LIMIT as usize]).unwrap();
        assert!(load(&root).is_ok());
        fs::write(&path, vec![b' '; LIMIT as usize + 1]).unwrap();
        assert!(load(&root).unwrap_err().message.contains("64 KiB"));
        fs::write(&path, b"\xff").unwrap();
        assert!(load(&root).unwrap_err().message.contains("UTF-8"));
    }

    #[test]
    fn saved_edits_keep_comments_unrelated_entries_and_modes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(NAME);
        let original = concat!(
            "# profile\n[recovery]\n# cadence rationale\ncheckpoint_interval_seconds = 5 # cadence\n",
            "[keybindings.global]\n# save rationale\nsave = [\n 'Ctrl+S',\n # alternate key\n 'F9',\n] # choices\n",
            "open = ['Ctrl+O'] # leave this spelling\n",
        );
        fs::write(&path, original).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let (mut draft, base) = load(root.path()).unwrap();
        draft.values.set(Scalar::Checkpoint, None);
        draft
            .map
            .set_override(Context::Global, "save", vec!["Ctrl+S".parse().unwrap()])
            .unwrap();
        draft.map.revalidate().unwrap();
        let (applied, new_base) = save(root.path(), &base, draft.clone(), || true).unwrap();
        assert_eq!((applied, new_base), load(root.path()).unwrap());
        let output = fs::read_to_string(&path).unwrap();
        for comment in [
            "# profile",
            "# cadence rationale",
            "# cadence",
            "# save rationale",
            "# alternate key",
            "# choices",
        ] {
            assert!(output.contains(comment), "missing {comment} in {output}");
        }
        assert!(output.contains("open = ['Ctrl+O'] # leave this spelling"));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        assert_eq!(parse(&output).unwrap(), draft);

        let new = root.path().join("new");
        let (applied, base) = save(&new, &Base::Missing, editing(), || true).unwrap();
        assert_eq!((applied, base), load(&new).unwrap());
        assert_eq!(fs::metadata(new.join(NAME)).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn initial_guard_refuses_changes_that_precede_opening_the_draft() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(NAME);
        let prior = "# original\n";
        fs::write(&path, prior).unwrap();
        let (_, base) = load(root.path()).unwrap();
        fs::write(&path, "# external edit before opening draft\n").unwrap();
        let tried = save(root.path(), &base, editing(), || {
            panic!("stale draft reached grant")
        });
        assert!(tried.unwrap_err().message.contains("changed on disk"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# external edit before opening draft\n"
        );
    }

    #[test]
    fn final_guard_keeps_external_changes_and_discards_unpublished_temporary_files() {
        for change in [
            "bytes",
            "mode",
            "hardlink",
            "symlink",
            "replaced",
            "created",
            "directory",
        ] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("ede");
            fs::create_dir(&root).unwrap();
            let path = root.join(NAME);
            if change != "created" {
                fs::write(&path, "# unchanged\n").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            let (_, base) = load(&root).unwrap();
            let (ready, staged) = mpsc::sync_channel(0);
            let (release, wait) = mpsc::sync_channel(0);
            std::thread::scope(|scope| {
                let write = scope.spawn(|| {
                    save(&root, &base, editing(), move || {
                        ready.send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(3)).unwrap();
                        true
                    })
                });
                staged.recv_timeout(Duration::from_secs(3)).unwrap();
                let before = match change {
                    "bytes" => {
                        fs::write(&path, "# new bytes\n").unwrap();
                        "# new bytes\n"
                    }
                    "mode" => {
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
                        "# unchanged\n"
                    }
                    "hardlink" => {
                        fs::hard_link(&path, root.join("alias")).unwrap();
                        "# unchanged\n"
                    }
                    "symlink" => {
                        fs::rename(&path, root.join("target")).unwrap();
                        symlink(root.join("target"), &path).unwrap();
                        "# unchanged\n"
                    }
                    "replaced" => {
                        fs::rename(&path, root.join("previous")).unwrap();
                        fs::write(&path, "# unchanged\n").unwrap();
                        "# unchanged\n"
                    }
                    "created" => {
                        fs::write(&path, "# newly created\n").unwrap();
                        "# newly created\n"
                    }
                    "directory" => {
                        fs::rename(&root, parent.path().join("parked")).unwrap();
                        fs::create_dir(&root).unwrap();
                        fs::write(&path, "# new directory\n").unwrap();
                        "# new directory\n"
                    }
                    _ => unreachable!(),
                };
                release.send(()).unwrap();
                let result = write.join().unwrap();
                assert_eq!(
                    fs::read_to_string(&path).unwrap(),
                    before,
                    "{change}: external file changed"
                );
                assert!(
                    !result.unwrap_err().uncertain,
                    "{change}: published before noticing change"
                );
                if change == "mode" {
                    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
                }
                let stage_dir = if change == "directory" {
                    parent.path().join("parked")
                } else {
                    root.clone()
                };
                for entry in fs::read_dir(stage_dir).unwrap() {
                    assert!(
                        !entry
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .ends_with(".tmp")
                    );
                }
            });
        }
    }

    #[test]
    fn lock_contention_and_revoked_grant_never_publish() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(NAME);
        fs::write(&path, "# before\n").unwrap();
        let (_, base) = load(root.path()).unwrap();
        let dir = directory(root.path()).unwrap();
        let guard = lock(&dir).unwrap();
        assert!(
            save(root.path(), &base, editing(), || panic!(
                "busy writer reached grant"
            ))
            .unwrap_err()
            .message
            .contains("Busy")
        );
        drop(guard);
        let cancelled = save(root.path(), &base, editing(), || false).unwrap_err();
        assert!(cancelled.message.contains("cancelled"));
        assert!(!cancelled.uncertain);
        assert_eq!(fs::read_to_string(&path).unwrap(), "# before\n");
    }

    #[test]
    fn failure_after_publish_reports_uncertainty() {
        let root = tempfile::tempdir().unwrap();
        let failed = save_with(
            root.path(),
            &Base::Missing,
            editing(),
            || true,
            || {},
            |_| Err(io::Error::other("injected directory sync failure")),
        )
        .unwrap_err();
        assert!(failed.uncertain);
        assert!(failed.message.contains("file may have changed"));
        assert_eq!(load(root.path()).unwrap().0, editing());
    }

    #[test]
    fn publication_from_absence_never_clobbers_a_new_name() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(NAME);
        let result = save_with(
            root.path(),
            &Base::Missing,
            editing(),
            || true,
            || {
                fs::write(&path, "# created after check\n").unwrap();
            },
            |_| Ok(()),
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "# created after check\n",
            "no-clobber publication replaced a new file"
        );
        assert!(!result.unwrap_err().uncertain);
    }

    #[test]
    fn staged_bytes_stay_private_and_cannot_change_before_publication() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(NAME);
        fs::write(&path, "# original\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        let (_, base) = load(root.path()).unwrap();
        let result = save_with(
            root.path(),
            &base,
            editing(),
            || true,
            || {
                let temp = fs::read_dir(root.path())
                    .unwrap()
                    .filter_map(Result::ok)
                    .find(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                    .unwrap();
                assert!(
                    temp.file_type().unwrap().is_dir(),
                    "the candidate must be in a private directory"
                );
                assert_eq!(temp.metadata().unwrap().mode() & 0o777, 0o700);
                fs::write(temp.path().join(STAGED_NAME), "# unvalidated replacement\n").unwrap();
            },
            |_| Ok(()),
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "# original\n",
            "unvalidated staged bytes reached the destination"
        );
        assert!(!result.unwrap_err().uncertain);
    }
}
