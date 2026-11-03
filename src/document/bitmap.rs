//! Bounded mutable disk bitmap for transient reachability/accounting scans.

use super::Error;
use std::{fs::File, os::unix::fs::FileExt, path::Path};

const BLOCK: usize = 4096;
const CACHE_BLOCKS: usize = 8;
/// One dirty disk block; a tiny bounded cache avoids one-byte I/O for fragmented trees.
struct Block {
    number: u64,
    bytes: Box<[u8; BLOCK]>,
    dirty: bool,
}
/// Byte-per-identity flags with sparse disk overflow and at most 32 KiB resident data.
pub(super) struct Bitmap {
    file: File,
    length: u64,
    cache: Vec<Block>,
    next: usize,
}
impl Bitmap {
    /// Create a private transient bitmap. Sparse zero pages represent unvisited identities.
    pub fn new(directory: &Path, length: u64) -> Result<Self, Error> {
        let file = tempfile::tempfile_in(directory)?;
        file.set_len(length)?;
        Ok(Self {
            file,
            length,
            cache: Vec::with_capacity(CACHE_BLOCKS),
            next: 0,
        })
    }
    /// Set selected bits on a range, counting entries that did not already have skip bits.
    pub fn mark(&mut self, mut start: u64, end: u64, mark: u8, skip: u8) -> Result<u64, Error> {
        if start > end || end > self.length {
            return Err(Error::CorruptStorage);
        }
        let mut changed = 0;
        while start < end {
            let block = start / BLOCK as u64;
            let within = (start % BLOCK as u64) as usize;
            let count = (end - start).min((BLOCK - within) as u64) as usize;
            let index = self.load(block)?;
            let entry = &mut self.cache[index];
            for byte in &mut entry.bytes[within..within + count] {
                if *byte & skip == 0 {
                    *byte |= mark;
                    changed += 1;
                    entry.dirty = true;
                }
            }
            start += count as u64;
        }
        Ok(changed)
    }
    /// Load one block, writing a dirty victim before reusing its bounded allocation.
    fn load(&mut self, number: u64) -> Result<usize, Error> {
        if let Some(index) = self.cache.iter().position(|block| block.number == number) {
            return Ok(index);
        }
        let mut block = if self.cache.len() < CACHE_BLOCKS {
            Block {
                number,
                bytes: Box::new([0; BLOCK]),
                dirty: false,
            }
        } else {
            let victim = &mut self.cache[self.next];
            if victim.dirty {
                let offset = victim.number * BLOCK as u64;
                let count = (self.length - offset).min(BLOCK as u64) as usize;
                self.file.write_all_at(&victim.bytes[..count], offset)?;
            }
            std::mem::replace(
                victim,
                Block {
                    number,
                    bytes: Box::new([0; BLOCK]),
                    dirty: false,
                },
            )
        };
        block.number = number;
        block.dirty = false;
        block.bytes.fill(0);
        let offset = number * BLOCK as u64;
        let count = (self.length - offset).min(BLOCK as u64) as usize;
        self.file.read_exact_at(&mut block.bytes[..count], offset)?;
        if self.cache.len() < CACHE_BLOCKS {
            self.cache.push(block);
            Ok(self.cache.len() - 1)
        } else {
            let index = self.next;
            self.cache[index] = block;
            self.next = (self.next + 1) % CACHE_BLOCKS;
            Ok(index)
        }
    }
}
