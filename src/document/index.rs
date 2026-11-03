//! Streaming format validation and disk-backed LF prefix counts per text block.

use super::{Error, Format, Newline};
use std::{fs::File, os::unix::fs::FileExt};

/// Disk prefix records: one u64 per block, not one allocation per line.
pub(super) struct LineIndex {
    file: File,
    page: u64,
    total: u64,
}
impl LineIndex {
    /// Initialize prefix zero for an empty source.
    pub fn new(file: File, page: usize) -> Result<Self, Error> {
        file.write_all_at(&0u64.to_le_bytes(), 0)?;
        Ok(Self {
            file,
            page: page as u64,
            total: 0,
        })
    }
    /// Count bytes being appended, recording only completed block prefixes.
    pub fn append(&mut self, start: u64, mut bytes: &[u8]) -> Result<(), Error> {
        let mut offset = start;
        while !bytes.is_empty() {
            let count = (self.page - offset % self.page).min(bytes.len() as u64) as usize;
            self.total = self
                .total
                .checked_add(memchr::memchr_iter(b'\n', &bytes[..count]).count() as u64)
                .ok_or(Error::ResourceLimit("line count overflow"))?;
            offset += count as u64;
            if offset.is_multiple_of(self.page) {
                self.file
                    .write_all_at(&self.total.to_le_bytes(), offset / self.page * 8)?;
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }
    /// LF total preceding a complete physical block, validated against the source length.
    pub fn prefix(&self, block: u64, len: u64) -> Result<u64, Error> {
        if block > len / self.page {
            return Err(Error::CorruptStorage);
        }
        let mut bytes = [0; 8];
        self.file.read_exact_at(
            &mut bytes,
            block.checked_mul(8).ok_or(Error::CorruptStorage)?,
        )?;
        let count = u64::from_le_bytes(bytes);
        if count > block * self.page {
            return Err(Error::CorruptStorage);
        }
        Ok(count)
    }
    /// Reset failed unpublished appends to the previous accepted source length/count.
    pub fn rollback(&mut self, length: u64, total: u64) -> Result<(), Error> {
        self.total = total;
        self.file.set_len((length / self.page + 1) * 8)?;
        Ok(())
    }
    /// Duplicate immutable-prefix index access for an independent snapshot reader.
    pub fn reader(&self) -> Result<Self, Error> {
        Ok(Self {
            file: self.file.try_clone()?,
            page: self.page,
            total: self.total,
        })
    }
    /// LF count of all currently accepted appended bytes.
    pub fn total(&self) -> u64 {
        self.total
    }
    /// Current on-disk index size.
    pub fn disk_bytes(&self, len: u64) -> u64 {
        (len / self.page + 1) * 8
    }
}

/// Incremental validator with at most three carried scalar bytes.
pub(super) struct Validator {
    carry: Vec<u8>,
    raw: u64,
    first: bool,
    pub format: Format,
    first_lf: bool,
    previous_cr: bool,
    line_width: u64,
    pub longest: u64,
}
impl Validator {
    /// Prepare a new raw-source validation pass, including BOM detection.
    pub fn new() -> Self {
        Self {
            carry: Vec::with_capacity(4),
            raw: 0,
            first: true,
            format: Format::default(),
            first_lf: true,
            previous_cr: false,
            line_width: 0,
            longest: 0,
        }
    }
    /// Validate a chunk. The first chunk must contain at least four bytes unless EOF.
    pub fn feed(&mut self, bytes: &[u8], eof: bool) -> Result<(), Error> {
        let base = self
            .raw
            .checked_sub(self.carry.len() as u64)
            .ok_or(Error::CorruptStorage)?;
        let mut joined = Vec::with_capacity(self.carry.len() + bytes.len());
        joined.extend_from_slice(&self.carry);
        joined.extend_from_slice(bytes);
        self.carry.clear();
        let mut skip = 0;
        if self.first {
            self.first = false;
            if joined.starts_with(&[0xff, 0xfe])
                || joined.starts_with(&[0xfe, 0xff])
                || joined.starts_with(&[0, 0, 0xfe, 0xff])
            {
                return Err(Error::UnsupportedEncoding { offset: 0 });
            }
            if joined.starts_with(&[0xef, 0xbb, 0xbf]) {
                self.format.bom = true;
                skip = 3;
            }
        }
        if memchr::memchr(0, &joined[skip..]).is_some() {
            return Err(Error::Binary);
        }
        if let Err(error) = std::str::from_utf8(&joined[skip..]) {
            if error.error_len().is_none() && !eof {
                self.carry
                    .extend_from_slice(&joined[skip + error.valid_up_to()..]);
            } else {
                return Err(Error::UnsupportedEncoding {
                    offset: base + skip as u64 + error.valid_up_to() as u64,
                });
            }
        }
        let mut begin = skip;
        for lf in memchr::memchr_iter(b'\n', &joined[skip..]) {
            let lf = skip + lf;
            if self.first_lf {
                let cr = if lf == begin {
                    self.previous_cr
                } else {
                    joined[lf - 1] == b'\r'
                };
                self.format.newline = if cr { Newline::CrLf } else { Newline::Lf };
                self.first_lf = false;
            }
            self.line_width = self
                .line_width
                .checked_add((lf - begin) as u64)
                .ok_or(Error::ResourceLimit("line width overflow"))?;
            self.longest = self.longest.max(self.line_width);
            self.line_width = 0;
            begin = lf + 1;
        }
        // Carried UTF-8 bytes have already been counted as raw bytes; avoid recounting them.
        self.line_width = self
            .line_width
            .checked_add((joined.len() - begin - self.carry.len()) as u64)
            .ok_or(Error::ResourceLimit("line width overflow"))?;
        self.longest = self.longest.max(self.line_width);
        self.previous_cr = joined.last() == Some(&b'\r');
        self.raw = self
            .raw
            .checked_add(bytes.len() as u64)
            .ok_or(Error::ResourceLimit("source length overflow"))?;
        Ok(())
    }
}
