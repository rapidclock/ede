//! Shared bounded clock cache. Entries are owned by the cache, never pinned by callers.

use super::Error;
use std::{collections::HashMap, fs::File, os::unix::fs::FileExt};

/// Page identity includes the store generation and file kind, preventing aliasing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct Key(pub u64, pub u8, pub u64);

/// A page and its second-chance reference bit.
struct Entry {
    bytes: Vec<u8>,
    used: bool,
}

/// Byte-accounted cache with a clock eviction ring bounded by admitted pages.
pub(super) struct Cache {
    entries: HashMap<Key, Entry>,
    ring: Vec<Key>,
    hand: usize,
    bytes: usize,
    limit: usize,
}
impl Cache {
    /// Create an empty cache. Entry overhead is charged conservatively.
    pub fn new(limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            ring: Vec::new(),
            hand: 0,
            bytes: 0,
            limit,
        }
    }
    /// Resident allocation charge, including ring/hash entry allowance.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    /// Copy a disk page into a caller buffer without exposing/pinning cache allocations.
    pub fn read(
        &mut self,
        key: Key,
        file: &File,
        offset: u64,
        page: usize,
        out: &mut [u8],
    ) -> Result<(), Error> {
        if !self.entries.contains_key(&key) {
            let charge = page + 256;
            if charge > self.limit {
                file.read_exact_at(out, offset)?;
                return Ok(());
            }
            while self.bytes + charge > self.limit {
                self.evict();
            }
            let mut bytes = vec![0; page];
            file.read_exact_at(&mut bytes, offset)?;
            self.entries.insert(key, Entry { bytes, used: true });
            self.ring.push(key);
            self.bytes += charge;
        }
        let entry = self.entries.get_mut(&key).ok_or(Error::CorruptStorage)?;
        entry.used = true;
        out.copy_from_slice(&entry.bytes[..out.len()]);
        Ok(())
    }
    /// Copy a subrange of an already cached immutable text page, if present.
    pub fn get(&mut self, key: Key, within: usize, out: &mut [u8]) -> bool {
        let Some(entry) = self.entries.get_mut(&key) else {
            return false;
        };
        let Some(bytes) = entry.bytes.get(within..within + out.len()) else {
            return false;
        };
        out.copy_from_slice(bytes);
        entry.used = true;
        true
    }
    /// Insert one complete immutable text page after a bounded positional read.
    pub fn insert(&mut self, key: Key, bytes: Vec<u8>) {
        if self.entries.contains_key(&key) {
            return;
        }
        let charge = bytes.len() + 256;
        if charge > self.limit {
            return;
        }
        while self.bytes + charge > self.limit {
            self.evict();
        }
        self.bytes += charge;
        self.entries.insert(key, Entry { bytes, used: true });
        self.ring.push(key);
    }
    /// Drop a generation after rollback/compaction; cached unpublished suffixes cannot be reused.
    pub fn discard_generation(&mut self, generation: u64) {
        self.entries.retain(|key, entry| {
            if key.0 == generation {
                self.bytes -= entry.bytes.len() + 256;
                false
            } else {
                true
            }
        });
        self.ring.retain(|key| key.0 != generation);
        self.hand = 0;
    }
    /// Remove one cold page; every referenced page gets at most one second chance.
    fn evict(&mut self) {
        loop {
            if self.hand >= self.ring.len() {
                self.hand = 0;
            }
            let key = self.ring[self.hand];
            let entry = self
                .entries
                .get_mut(&key)
                .expect("clock keys remain present");
            if entry.used {
                entry.used = false;
                self.hand += 1;
                continue;
            }
            self.bytes -= entry.bytes.len() + 256;
            self.entries.remove(&key);
            self.ring.swap_remove(self.hand);
            return;
        }
    }
}
