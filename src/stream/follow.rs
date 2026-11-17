//! Polling pathname followers. Published text is never read from a mutable log.

use crate::document::{Error, FileVersion, open_regular};
use std::{
    fs::{self, File},
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const INTERVAL: Duration = Duration::from_millis(250);
const WITNESS: usize = 4096;

pub(super) enum Read {
    Bytes(usize, Option<String>),
    Idle(Option<String>),
    Seam(&'static str),
}

struct Generation {
    file: File,
    resolved: PathBuf,
    version: FileVersion,
    offset: u64,
    witness: Vec<u8>,
}

impl Generation {
    fn open(path: &Path) -> Result<Self, Error> {
        let (file, version) = open_regular(path)?;
        let resolved = fs::canonicalize(path)?;
        if !FileVersion::of(&fs::metadata(&resolved)?).same_object(&version) {
            return Err(Error::SourceChanged);
        }
        Ok(Self {
            file,
            resolved,
            version,
            offset: 0,
            witness: Vec::with_capacity(WITNESS),
        })
    }

    fn changed_at_cursor(&self, observed: &FileVersion) -> Result<bool, Error> {
        if observed.len < self.offset || observed.len < self.version.len {
            return Ok(true);
        }
        if self.witness.is_empty() || !observed.content_changed(&self.version) {
            return Ok(false);
        }
        let mut bytes = [0; WITNESS];
        let count = self.witness.len();
        match self
            .file
            .read_exact_at(&mut bytes[..count], self.offset - count as u64)
        {
            Ok(()) => Ok(bytes[..count] != self.witness),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(true),
            Err(e) => Err(e.into()),
        }
    }

    fn rewind(&mut self, version: FileVersion) {
        self.offset = 0;
        self.witness.clear();
        self.version = version;
    }

    fn advance(&mut self, bytes: &[u8], version: FileVersion) {
        self.offset += bytes.len() as u64;
        if bytes.len() >= WITNESS {
            self.witness.clear();
            self.witness
                .extend_from_slice(&bytes[bytes.len() - WITNESS..]);
        } else {
            let remove = (self.witness.len() + bytes.len()).saturating_sub(WITNESS);
            self.witness.drain(..remove);
            self.witness.extend_from_slice(bytes);
        }
        self.version = version;
    }
}

pub(super) struct Follower {
    path: PathBuf,
    current: Generation,
    replacement: Option<(Generation, u64)>,
    next_check: Instant,
    warning: Option<String>,
}

impl Follower {
    pub fn resolved(&self) -> &Path {
        &self.current.resolved
    }
    pub fn open(path: PathBuf) -> Result<Self, Error> {
        Ok(Self {
            current: Generation::open(&path)?,
            path,
            replacement: None,
            next_check: Instant::now() + INTERVAL,
            warning: None,
        })
    }

    /// Check the path on a fixed cadence even if the old descriptor still has readable data.
    fn observe(&mut self, observed: &FileVersion) {
        if self.replacement.is_some() || Instant::now() < self.next_check {
            return;
        }
        self.next_check = Instant::now() + INTERVAL;
        let checked = (|| {
            let named = fs::metadata(&self.path)?;
            if !FileVersion::of(&named).same_object(observed) {
                let replacement = Generation::open(&self.path)?;
                if !replacement.version.same_object(observed) {
                    let cutoff = self.current.file.metadata()?.len();
                    self.replacement = Some((replacement, cutoff));
                }
            }
            Ok::<_, Error>(())
        })();
        self.warning = checked
            .err()
            .map(|error| format!("Follow path unavailable: {error}"));
    }

    fn restart(&mut self, version: FileVersion) -> Read {
        if self.replacement.is_some() {
            self.switch("Rotation with a gap in the old file")
        } else {
            self.current.rewind(version);
            Read::Seam("File truncated or rewritten")
        }
    }

    fn switch(&mut self, reason: &'static str) -> Read {
        let (next, _) = self.replacement.take().expect("open replacement");
        self.current = next;
        self.next_check = Instant::now();
        Read::Seam(reason)
    }

    /// Observe the witness around bounded positional reads, discarding superseded data.
    pub fn read(&mut self, buffer: &mut [u8]) -> Result<Read, Error> {
        let before = FileVersion::of(&self.current.file.metadata()?);
        self.observe(&before);
        if self.current.changed_at_cursor(&before)? {
            return Ok(self.restart(before));
        }
        self.current.version = before.clone();
        let mut end = before.len;
        if let Some((_, cutoff)) = &self.replacement {
            if self.current.offset >= *cutoff {
                return Ok(self.switch("File rotated"));
            }
            end = end.min(*cutoff);
        }
        let count = (end - self.current.offset).min(buffer.len() as u64) as usize;
        if count == 0 {
            return Ok(Read::Idle(self.warning.clone()));
        }
        let read = loop {
            match self
                .current
                .file
                .read_at(&mut buffer[..count], self.current.offset)
            {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                read => break read?,
            }
        };
        let after = FileVersion::of(&self.current.file.metadata()?);
        if after.len < self.current.offset + read as u64
            || self.current.changed_at_cursor(&after)?
        {
            return Ok(self.restart(after));
        }
        if read == 0 {
            return Ok(Read::Idle(self.warning.clone()));
        }
        self.current.advance(&buffer[..read], after);
        Ok(Read::Bytes(read, self.warning.clone()))
    }
}
