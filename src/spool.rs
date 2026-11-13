//! Private disk-backed clipboard/paste bytes with bounded streaming readers.
use std::{
    fs::File,
    io::{self, Read, Write},
    os::unix::fs::FileExt,
};

/// Immutable paste/clipboard after staging; dropping the last owner removes its file.
#[derive(Debug)]
pub(crate) struct Spool {
    file: File,
    pub len: u64,
}
impl Spool {
    /// Create a private, unlinked temporary file. No system clipboard access is implied.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            file: tempfile::tempfile()?,
            len: 0,
        })
    }
    /// Append a bounded chunk after checking available disk space and the reserve.
    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let space = nix::sys::statvfs::fstatvfs(&self.file)?;
        if (space.blocks_available() as u128) * (space.fragment_size() as u128)
            < (512 << 20) + bytes.len() as u128
        {
            return Err(io::Error::other("Insufficient temporary disk space"));
        }
        self.file.write_all(bytes)?;
        self.len = self
            .len
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("Paste length overflow"))?;
        Ok(())
    }
    /// Read without changing another user's file position (clipboard reads may overlap).
    pub fn reader(&self) -> impl Read + '_ {
        Reader {
            spool: self,
            offset: 0,
        }
    }
}
/// Independent forward-only file cursor; resident memory does not scale with the payload.
struct Reader<'a> {
    spool: &'a Spool,
    offset: u64,
}
impl Read for Reader<'_> {
    /// Use positional reads so an Arc-shared clipboard stays immutable.
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.spool.file.read_at(output, self.offset)?;
        self.offset += count as u64;
        Ok(count)
    }
}
