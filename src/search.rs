//! Bounded literal Unicode search over immutable paged document revisions.
//!
//! Scanning never reads the named file and never retains all matches. Callbacks
//! run on the calling storage/search worker; terminal handlers must not call this API.
use crate::document::{ByteOffset, Cancellation, Error, Snapshot, TextRange};
use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};

/// Maximum UTF-8 field size, independent of the document or line length.
pub const FIELD_BYTES: usize = 4096;
/// Captured document capability; active streams remain read-only until EOF.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Capability {
    /// Ready ordinary or scratch text, including finite stdin after EOF.
    #[default]
    Editable,
    /// Retained stream history supports search but never replacement/editing.
    ReadOnly,
}
/// Conservative reservation covering matcher, caches, worker buffers and summaries.
pub(crate) const WORK_BYTES: usize = 8 << 20;

/// Literal query semantics shared by counting, navigation, and replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    /// Exact query text. An empty query yields no matches.
    pub literal: String,
    /// Compare scalars exactly when true, or use Unicode simple folding otherwise.
    pub match_case: bool,
    /// Require non-word adjacent scalars using Unicode regex word properties.
    pub whole_word: bool,
}
impl Default for Query {
    fn default() -> Self {
        Self {
            literal: String::new(),
            match_case: true,
            whole_word: false,
        }
    }
}
impl Query {
    /// Compile only literal syntax with bounded program/cache sizes.
    pub fn compile(&self) -> Result<Literal, Error> {
        validate_field(&self.literal)?;
        let scalars = self.literal.chars().count();
        // A long needle must not repeatedly try full matches that begin only
        // in lookahead. Locate its first scalar within the new scan region,
        // then verify the escaped literal anchored at that original offset.
        let start = if scalars > 64 {
            Some(
                RegexMatcherBuilder::new()
                    .fixed_strings(true)
                    .case_insensitive(!self.match_case)
                    .size_limit(512 << 10)
                    .dfa_size_limit(128 << 10)
                    .build(
                        &self
                            .literal
                            .chars()
                            .next()
                            .expect("nonempty long query")
                            .to_string(),
                    )
                    .map_err(|_| Error::ResourceLimit("search prefix compilation"))?,
            )
        } else {
            None
        };
        let pattern = if start.is_some() {
            format!("\\A(?:{})", regex_syntax::escape(&self.literal))
        } else {
            self.literal.clone()
        };
        let matcher = RegexMatcherBuilder::new()
            .fixed_strings(start.is_none())
            .case_insensitive(!self.match_case)
            .size_limit(2 << 20)
            .dfa_size_limit(1 << 20)
            .build(&pattern)
            .map_err(|_| Error::ResourceLimit("search matcher compilation"))?;
        Ok(Literal {
            matcher,
            start,
            whole_word: self.whole_word,
            scalars,
        })
    }
}

/// Validate literal query/replacement input without changing the previous field.
pub fn validate_field(text: &str) -> Result<(), Error> {
    if text.len() > FIELD_BYTES {
        return Err(Error::ResourceLimit("search field length (4096 bytes)"));
    }
    if text.contains('\0') {
        return Err(Error::InvalidRange);
    }
    Ok(())
}

/// Testable scan geometry; production uses at most 64 KiB of new text per slice.
#[derive(Clone, Copy, Debug)]
pub struct ScanOptions {
    /// Maximum new bytes before progress/cancellation; clamped to 4..=65536.
    pub chunk_bytes: usize,
    /// Optional exclusive candidate-start bound for viewport-derived work.
    /// Context past this offset is still read to validate whole matches.
    pub stop_after: Option<u64>,
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            chunk_bytes: 64 << 10,
            stop_after: None,
        }
    }
}

/// Coalescible cumulative progress for one captured revision.
#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    /// Canonical nonoverlapping matches encountered so far.
    pub matches: u64,
    /// Logical bytes scanned; a complete scan reaches total.
    pub scanned: u64,
    /// Captured logical document extent.
    pub total: u64,
}

/// Compiled literal; original byte offsets survive Unicode folding.
pub struct Literal {
    matcher: RegexMatcher,
    start: Option<RegexMatcher>,
    whole_word: bool,
    scalars: usize,
}
impl Literal {
    /// Stream canonical matches, stopping when the callback returns false.
    ///
    /// Accepted matches resume at their end. Rejected boundary candidates resume
    /// one scalar after their start, preserving later overlapping valid matches.
    pub fn scan(
        &self,
        snapshot: &Snapshot,
        cancel: &Cancellation,
        options: ScanOptions,
        matched: impl FnMut(TextRange) -> Result<bool, Error>,
        progress: impl FnMut(Progress),
    ) -> Result<Progress, Error> {
        let _permit = snapshot.reserve_work(WORK_BYTES)?;
        self.scan_admitted(snapshot, cancel, options, matched, progress)
    }

    /// A session worker holds its full reservation before compiling and until
    /// publication/mapping completes; avoid charging that same allocation twice.
    pub(crate) fn scan_admitted(
        &self,
        snapshot: &Snapshot,
        cancel: &Cancellation,
        options: ScanOptions,
        mut matched: impl FnMut(TextRange) -> Result<bool, Error>,
        mut progress: impl FnMut(Progress),
    ) -> Result<Progress, Error> {
        let mut result = Progress {
            total: snapshot.len(),
            ..Progress::default()
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if self.scalars == 0 {
            result.scanned = result.total;
            progress(result);
            return Ok(result);
        }
        let chunk = options.chunk_bytes.clamp(4, 64 << 10);
        let scan_end = snapshot
            .floor_scalar(ByteOffset(
                options
                    .stop_after
                    .unwrap_or(snapshot.len())
                    .min(snapshot.len()),
            ))?
            .0;
        let lookahead = self.scalars * 4 + 8;
        let mut offset = 0;
        while offset < scan_end {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let start = snapshot
                .floor_scalar(ByteOffset(offset.saturating_sub(4)))?
                .0;
            let stop = snapshot
                .floor_scalar(ByteOffset((offset + chunk as u64).min(scan_end)))?
                .0;
            let window = snapshot.read_window_with_cancel(
                ByteOffset(start),
                (offset - start) as usize + chunk + lookahead,
                cancel,
            )?;
            let text = window.text();
            let bytes = text.as_bytes();
            let mut at = (offset - start) as usize;
            let mut candidates: usize = 0;
            while at < bytes.len() {
                if candidates.is_multiple_of(256) && cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let Some(found) = self
                    .start
                    .as_ref()
                    .unwrap_or(&self.matcher)
                    .find_at(bytes, at)
                    .map_err(|_| Error::CorruptStorage)?
                else {
                    break;
                };
                if start + found.start() as u64 >= stop {
                    break;
                }
                candidates += 1;
                let a = found.start();
                let b = if self.start.is_some() {
                    match self
                        .matcher
                        .find(&bytes[a..])
                        .map_err(|_| Error::CorruptStorage)?
                    {
                        Some(verified) => a + verified.end(),
                        None => {
                            at = a + text[a..]
                                .chars()
                                .next()
                                .ok_or(Error::CorruptStorage)?
                                .len_utf8();
                            continue;
                        }
                    }
                } else {
                    found.end()
                };
                let crlf = |i: usize| {
                    i != 0 && i < bytes.len() && bytes[i - 1] == b'\r' && bytes[i] == b'\n'
                };
                let words = !self.whole_word
                    || (!text[..a]
                        .chars()
                        .next_back()
                        .is_some_and(regex_syntax::is_word_character)
                        && !text[b..]
                            .chars()
                            .next()
                            .is_some_and(regex_syntax::is_word_character));
                if !crlf(a) && !crlf(b) && words {
                    result.matches = result
                        .matches
                        .checked_add(1)
                        .ok_or(Error::ResourceLimit("search count overflow"))?;
                    result.scanned = start + b as u64;
                    if !matched(TextRange::new(start + a as u64, start + b as u64)?)? {
                        progress(result);
                        return Ok(result);
                    }
                    at = b;
                } else {
                    at = a + text[a..]
                        .chars()
                        .next()
                        .ok_or(Error::CorruptStorage)?
                        .len_utf8();
                }
                if candidates.is_multiple_of(256) {
                    progress(result);
                }
            }
            offset = stop.max(start + at as u64).min(snapshot.len());
            result.scanned = offset;
            progress(result);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(result)
    }
}
