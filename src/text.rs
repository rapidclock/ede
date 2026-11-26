//! Chunked Unicode navigation and a shared immutable text/cell map.
//!
//! All methods reading snapshots run on the editor worker. Only visible spans
//! are retained; checkpoints are sparse, disposable, and revision tagged.

use crate::document::{ByteOffset, Cancellation, Error, Revision, Snapshot};
use std::{
    collections::VecDeque,
    ops::Range,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Duration,
};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};
use unicode_width::UnicodeWidthStr;

/// Maximum scalar-aligned text window, independent of line or grapheme length.
const WINDOW: usize = 64 << 10;
/// Maximum bytes materialized for one rendered grapheme.
const GLYPH_BYTES: u64 = 1024;

/// Logical selection and caret; anchor preserves selection direction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Position {
    pub caret: u64,
    pub anchor: u64,
}
impl Position {
    /// Selected logical bytes without materializing their contents.
    pub fn selection(self) -> Range<u64> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }
    /// Set the active end, optionally starting or extending a selection.
    pub fn move_to(&mut self, offset: u64, extend: bool) {
        self.caret = offset;
        if !extend {
            self.anchor = offset;
        }
    }
}

/// One atomic token or run of equal-byte/equal-cell graphemes, shared by drawing and hits.
#[derive(Debug)]
pub(crate) struct Span {
    pub bytes: Range<u64>,
    pub column: u64,
    pub width: u64,
    pub text: String,
    /// Byte/cell strides for a prepared run; None keeps an escaped token atomic.
    pub unit: Option<(u16, u16)>,
}
/// Prepared visible logical line; does not retain the offscreen line's text.
#[derive(Debug)]
pub(crate) struct Row {
    pub end: u64,
    pub spans: Box<[Span]>,
}
impl Row {
    /// Nearest boundary with ties before a glyph; all occupied cells are atomic.
    pub fn hit(&self, column: u64) -> u64 {
        for glyph in &self.spans {
            if column < glyph.column + glyph.width {
                if let Some((bytes, cells)) = glyph.unit {
                    let delta = column.saturating_sub(glyph.column);
                    let cells = u64::from(cells);
                    let index = delta / cells + u64::from(delta % cells * 2 > cells);
                    return glyph.bytes.start + index * u64::from(bytes);
                }
                return if column.saturating_sub(glyph.column) * 2 <= glyph.width {
                    glyph.bytes.start
                } else {
                    glyph.bytes.end
                };
            }
        }
        self.end
    }
}
/// Immutable viewport result. Geometry and revision guard every pointer hit.
#[derive(Debug)]
pub(crate) struct View {
    _lease: LayoutLease,
    pub revision: Revision,
    pub generation: u64,
    pub width: u16,
    pub height: u16,
    pub top: u64,
    pub left: u64,
    pub rows: Vec<Row>,
    pub cursor: Option<(u16, u16)>,
    pub line: u64,
    pub column: u64,
}

/// At most two live layout allocations (3 MiB each), including in-flight construction.
/// Shared Arcs do not consume extra slots; dropping the final View releases one.
#[derive(Debug)]
struct LayoutLease {
    charged: usize,
}
static LIVE_LAYOUTS: AtomicUsize = AtomicUsize::new(0);
static LAYOUT_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_LAYOUT_BYTES: AtomicUsize = AtomicUsize::new(0);
impl LayoutLease {
    /// Wait only on a background worker, observing cancellation while the UI swaps old frames.
    fn acquire(cancel: &Cancellation) -> Result<Self, Error> {
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if LIVE_LAYOUTS
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < 2).then_some(n + 1)
                })
                .is_ok()
            {
                return Ok(Self { charged: 0 });
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    /// Record conservative retained-allocation charges, including construction.
    fn charge(&mut self, bytes: usize) {
        if bytes != 0 {
            self.charged += bytes;
            let total = LAYOUT_BYTES.fetch_add(bytes, Ordering::AcqRel) + bytes;
            PEAK_LAYOUT_BYTES.fetch_max(total, Ordering::AcqRel);
        }
    }
    /// Return capacity released when a completed row is packed to its exact span count.
    fn release(&mut self, bytes: usize) {
        self.charged -= bytes;
        LAYOUT_BYTES.fetch_sub(bytes, Ordering::AcqRel);
    }
}
/// Admit actual descriptor capacity before growing; even the first Vec allocation is charged.
fn reserve_slot<T>(
    values: &mut Vec<T>,
    budget: &mut usize,
    lease: &mut LayoutLease,
) -> Result<(), Error> {
    if values.len() == values.capacity() {
        let next = values.capacity().saturating_mul(2).max(4);
        let charge = (next - values.capacity()) * std::mem::size_of::<T>();
        if charge > *budget {
            return Err(Error::ResourceLimit("visible layout budget"));
        }
        values
            .try_reserve_exact(next - values.len())
            .map_err(|_| Error::ResourceLimit("layout allocation"))?;
        *budget -= charge;
        lease.charge(charge);
    }
    Ok(())
}
impl Drop for LayoutLease {
    /// Retained screen results keep their admission reservation until the final Arc is dropped.
    fn drop(&mut self) {
        LAYOUT_BYTES.fetch_sub(self.charged, Ordering::AcqRel);
        LIVE_LAYOUTS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Bounded Unicode reader and sparse cell checkpoints for a single captured revision.
pub(crate) struct Text<'a> {
    snapshot: &'a Snapshot,
    cancel: &'a Cancellation,
    start: usize,
    chunk: String,
    cursor: Option<GraphemeCursor>,
}
impl<'a> Text<'a> {
    /// Empty reader; text windows are fetched on demand.
    pub fn new(snapshot: &'a Snapshot, cancel: &'a Cancellation) -> Self {
        Self {
            snapshot,
            cancel,
            start: 0,
            chunk: String::new(),
            cursor: None,
        }
    }
    /// Fetch an aligned window starting at or just before the requested byte.
    fn load(&mut self, start: usize) -> Result<(), Error> {
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.start = self.snapshot.floor_scalar(ByteOffset(start as u64))?.0 as usize;
        self.chunk = self
            .snapshot
            .read_window_with_cancel(ByteOffset(self.start as u64), WINDOW, self.cancel)?
            .text()
            .to_owned();
        Ok(())
    }
    /// Resolve one boundary request without ever joining windows into a long string.
    fn boundary(&mut self, offset: u64, direction: i8) -> Result<u64, Error> {
        let length = usize::try_from(self.snapshot.len())
            .map_err(|_| Error::ResourceLimit("address space"))?;
        let offset = usize::try_from(offset).map_err(|_| Error::InvalidRange)?;
        if offset > length {
            return Err(Error::InvalidRange);
        }
        let mut cursor = self
            .cursor
            .take()
            .filter(|cursor| cursor.cur_cursor() == offset)
            .unwrap_or_else(|| GraphemeCursor::new(offset, length, true));
        let usable = offset >= self.start
            && offset <= self.start + self.chunk.len()
            && if direction < 0 {
                offset > self.start
            } else {
                offset < self.start + self.chunk.len()
            };
        if !usable {
            self.load(if direction < 0 {
                offset.saturating_sub(WINDOW / 2)
            } else {
                offset
            })?;
        }
        loop {
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let result = if direction < 0 {
                cursor.prev_boundary(&self.chunk, self.start)
            } else {
                cursor.next_boundary(&self.chunk, self.start)
            };
            match result {
                Ok(Some(boundary)) => {
                    self.cursor = Some(cursor);
                    return Ok(boundary as u64);
                }
                Ok(None) => return Ok(if direction < 0 { 0 } else { length as u64 }),
                Err(GraphemeIncomplete::NextChunk) => self.load(self.start + self.chunk.len())?,
                Err(GraphemeIncomplete::PrevChunk) => {
                    self.load(self.start.saturating_sub(WINDOW / 2))?
                }
                Err(GraphemeIncomplete::PreContext(end)) => {
                    let saved_start = self.start;
                    let saved = std::mem::take(&mut self.chunk);
                    self.load(end.saturating_sub(WINDOW / 2))?;
                    let count = end.checked_sub(self.start).ok_or(Error::CorruptStorage)?;
                    cursor.provide_context(&self.chunk[..count], self.start);
                    self.start = saved_start;
                    self.chunk = saved;
                }
                Err(GraphemeIncomplete::InvalidOffset) => return Err(Error::InvalidRange),
            }
        }
    }
    /// Next extended grapheme boundary; CRLF is one unit under UAX 29.
    pub fn next(&mut self, offset: u64) -> Result<u64, Error> {
        self.boundary(offset, 1)
    }
    /// Previous extended grapheme boundary, including across chunk boundaries.
    pub fn previous(&mut self, offset: u64) -> Result<u64, Error> {
        self.boundary(offset, -1)
    }
    /// First valid caret boundary at or after an insertion endpoint.
    pub fn ceil(&mut self, offset: u64) -> Result<u64, Error> {
        if offset == 0 || offset == self.snapshot.len() {
            return Ok(offset);
        }
        let next = self.next(offset)?;
        let previous = self.previous(next)?;
        Ok(if previous == offset { offset } else { next })
    }
    /// Materialize only a bounded glyph; huge clusters become one labeled token.
    fn glyph(&mut self, start: u64, end: u64, column: u64) -> Result<Span, Error> {
        let text = if end - start > GLYPH_BYTES {
            "[cluster]".to_owned()
        } else {
            if start >= self.start as u64 && end <= (self.start + self.chunk.len()) as u64 {
                self.chunk[(start as usize - self.start)..(end as usize - self.start)].to_owned()
            } else {
                self.snapshot
                    .read_window_with_cancel(
                        ByteOffset(start),
                        (end - start).max(4) as usize,
                        self.cancel,
                    )?
                    .text()[..(end - start) as usize]
                    .to_owned()
            }
        };
        let mut unit = None;
        let (text, width) = if text == "\t" {
            (" ".repeat((4 - column % 4) as usize), 4 - column % 4)
        } else {
            let escaped = safe(&text);
            let width = escaped.width() as u64;
            if width == 0 {
                ("[zero]".to_owned(), 6)
            } else {
                if escaped == text && end - start <= GLYPH_BYTES {
                    unit = Some(((end - start) as u16, width as u16));
                }
                (escaped, width)
            }
        };
        Ok(Span {
            bytes: start..end,
            column,
            width,
            text,
            unit,
        })
    }
    /// Exact logical start/end, excluding a line's LF or CRLF terminator.
    pub fn line_bounds(&self, line: u64) -> Result<(u64, u64), Error> {
        let start = self.snapshot.locate_line(line, self.cancel)?.0;
        if line + 1 == self.snapshot.line_count() {
            return Ok((start, self.snapshot.len()));
        }
        let next = self.snapshot.locate_line(line + 1, self.cancel)?.0;
        let tail = self
            .snapshot
            .floor_scalar(ByteOffset(next.saturating_sub(2)))?
            .0
            .max(start);
        let window = self
            .snapshot
            // Rounding next-2 down can include a four-byte scalar followed by
            // LF (five bytes). Four bytes would omit the LF and panic below.
            .read_window_with_cancel(ByteOffset(tail), 8, self.cancel)?;
        let end = next
            - if window.text()[..(next - tail) as usize].ends_with("\r\n") {
                2
            } else {
                1
            };
        Ok((start, end))
    }
    /// Scan a logical line to a cell or byte target; only sparse checkpoints survive.
    fn scan(
        &mut self,
        line: u64,
        byte: Option<u64>,
        cell: Option<u64>,
        cache: &mut Checkpoints,
    ) -> Result<(u64, u64), Error> {
        cache.reset(self.snapshot.revision());
        let (start, end) = self.line_bounds(line)?;
        let (mut offset, mut column) = cache.best(line, byte, cell).unwrap_or((start, 0));
        let mut checkpoint = offset;
        while offset < end {
            if byte.is_some_and(|target| offset >= target)
                || cell.is_some_and(|target| column >= target)
            {
                break;
            }
            // Printable ASCII runs have one cell per byte. Leave their last byte
            // for the grapheme cursor when following Unicode could attach to it.
            if offset < self.start as u64 || offset >= (self.start + self.chunk.len()) as u64 {
                self.load(offset as usize)?;
            }
            let within = offset as usize - self.start;
            let ascii = self.chunk.as_bytes()[within..]
                .iter()
                .take_while(|byte| matches!(byte, b' '..=b'~'))
                .count() as u64;
            let available = ascii.min(end - offset);
            let safe_run = if offset + available == end {
                available
            } else {
                available.saturating_sub(1)
            };
            let run = safe_run
                .min(byte.map_or(u64::MAX, |target| target.saturating_sub(offset)))
                .min(cell.map_or(u64::MAX, |target| target.saturating_sub(column)));
            if run > 0 {
                offset += run;
                column = column.saturating_add(run);
                if offset - checkpoint >= WINDOW as u64 {
                    cache.insert(line, offset, column);
                    checkpoint = offset;
                }
                continue;
            }
            let next = self.next(offset)?.min(end);
            let glyph = self.glyph(offset, next, column)?;
            if let Some(target) = cell
                && column + glyph.width > target
            {
                return Ok(if (target - column) * 2 <= glyph.width {
                    (offset, column)
                } else {
                    (next, column + glyph.width)
                });
            }
            column = column.saturating_add(glyph.width);
            offset = next;
            if offset - checkpoint >= WINDOW as u64 {
                cache.insert(line, offset, column);
                checkpoint = offset;
            }
        }
        Ok((offset, column))
    }
    /// Cell coordinate of a valid caret, with a bounded checkpoint cache.
    pub(crate) fn grapheme_coordinates(&mut self, offset: u64) -> Result<(u64, u64), Error> {
        let line = self.snapshot.line_of_offset(ByteOffset(offset))?;
        Ok((line, self.scan_graphemes(line, Some(offset), None)?.1))
    }
    /// Clamp a logical grapheme ordinal to a line without depending on terminal cell widths.
    pub(crate) fn at_grapheme(&mut self, line: u64, ordinal: u64) -> Result<u64, Error> {
        Ok(self.scan_graphemes(line, None, Some(ordinal))?.0)
    }
    /// Count long ASCII runs in bulk and stream all other extended grapheme boundaries.
    fn scan_graphemes(
        &mut self,
        line: u64,
        byte: Option<u64>,
        ordinal: Option<u64>,
    ) -> Result<(u64, u64), Error> {
        let (mut offset, end) = self.line_bounds(line)?;
        let mut count = 0u64;
        while offset < end {
            if byte.is_some_and(|target| offset >= target)
                || ordinal.is_some_and(|target| count >= target)
            {
                break;
            }
            if offset < self.start as u64 || offset >= (self.start + self.chunk.len()) as u64 {
                self.load(offset as usize)?;
            }
            let within = offset as usize - self.start;
            let available = self.chunk.as_bytes()[within..]
                .iter()
                .take_while(|byte| matches!(byte, b' '..=b'~'))
                .count() as u64;
            let available = available.min(end - offset);
            let safe = if offset + available == end {
                available
            } else {
                available.saturating_sub(1)
            };
            let run = safe
                .min(byte.map_or(u64::MAX, |target| target.saturating_sub(offset)))
                .min(ordinal.map_or(u64::MAX, |target| target.saturating_sub(count)));
            if run != 0 {
                offset += run;
                count += run;
                continue;
            }
            let next = self.next(offset)?.min(end);
            if next <= offset {
                return Err(Error::CorruptStorage);
            }
            offset = next;
            count += 1;
        }
        Ok((offset, count))
    }
    /// Cell coordinate of a valid caret, with a bounded checkpoint cache.
    pub fn coordinates(
        &mut self,
        offset: u64,
        cache: &mut Checkpoints,
    ) -> Result<(u64, u64), Error> {
        let line = self.snapshot.line_of_offset(ByteOffset(offset))?;
        Ok((line, self.scan(line, Some(offset), None, cache)?.1))
    }
    /// Logical boundary closest to the requested cell on a line.
    pub fn at_column(
        &mut self,
        line: u64,
        column: u64,
        cache: &mut Checkpoints,
    ) -> Result<u64, Error> {
        Ok(self.scan(line, None, Some(column), cache)?.0)
    }
    /// Clamp an offset to the visible lines' actual ends, independent of a disposable layout job.
    pub fn clamp_left(
        &mut self,
        top: u64,
        height: u16,
        left: u64,
        cache: &mut Checkpoints,
    ) -> Result<u64, Error> {
        if left == 0 || height == 0 {
            return Ok(left);
        }
        let mut furthest = 0;
        for line in top..top
            .saturating_add(u64::from(height))
            .min(self.snapshot.line_count())
        {
            let (_, end) = self.line_bounds(line)?;
            let (byte, column) = self.scan(line, None, Some(left), cache)?;
            if column >= left || byte < end {
                return Ok(left);
            }
            furthest = furthest.max(column);
        }
        Ok(left.min(furthest))
    }
    /// Build only visible rows and spans, charging retained strings and descriptors.
    pub fn view(&mut self, request: ViewRequest, cache: &mut Checkpoints) -> Result<View, Error> {
        self.view_revealing_match(request, cache, None)
    }

    /// Reveal a match above the window while preparing the full document viewport.
    pub fn view_revealing_match(
        &mut self,
        request: ViewRequest,
        cache: &mut Checkpoints,
        search: Option<(u16, u64)>,
    ) -> Result<View, Error> {
        let mut lease = LayoutLease::acquire(self.cancel)?;
        let ViewRequest {
            position,
            mut top,
            mut left,
            width,
            height,
            reveal,
            generation,
        } = request;
        cache.reset(self.snapshot.revision());
        let (line, column) = self.coordinates(position.caret, cache)?;
        if reveal {
            let (visible, last) = if let Some((covered, last_byte)) = search {
                let visible = height.saturating_sub(covered).max(1);
                let last_byte = self.snapshot.floor_scalar(ByteOffset(last_byte))?;
                let last = self.snapshot.line_of_offset(last_byte)?;
                let last = if last.saturating_sub(line) < u64::from(visible) {
                    last.max(line)
                } else {
                    line
                };
                (visible, last)
            } else {
                (height, line)
            };
            if line < top {
                top = line;
            } else if last >= top.saturating_add(u64::from(visible)) {
                top = last.saturating_sub(u64::from(visible.saturating_sub(1)));
            }
            if column < left {
                left = column;
            } else if column >= left.saturating_add(width as u64) {
                left = column.saturating_sub(width.saturating_sub(1) as u64);
            }
        }
        top = top.min(self.snapshot.line_count().saturating_sub(1));
        let mut rows = Vec::new();
        let mut budget = 3 << 20;
        let mut furthest_column = 0;
        for number in top..top
            .saturating_add(height as u64)
            .min(self.snapshot.line_count())
        {
            let (_, end) = self.line_bounds(number)?;
            let (mut offset, mut col) = self.scan(number, None, Some(left), cache)?;
            furthest_column = furthest_column.max(col);
            // A nearest-boundary hit may choose the end of a partly clipped glyph.
            if col > left && offset > 0 {
                offset = self.previous(offset)?;
                col = self.scan(number, Some(offset), None, cache)?.1;
            }
            let mut spans: Vec<Span> = Vec::new();
            while offset < end && col < left.saturating_add(width as u64) {
                if offset < self.start as u64 || offset >= (self.start + self.chunk.len()) as u64 {
                    self.load(offset as usize)?;
                }
                let within = offset as usize - self.start;
                let ascii = self.chunk.as_bytes()[within..]
                    .iter()
                    .take_while(|byte| matches!(byte, b' '..=b'~'))
                    .count() as u64;
                let available = ascii.min(end - offset);
                let safe_run = if offset + available == end {
                    available
                } else {
                    available.saturating_sub(1)
                };
                let run = safe_run.min(left.saturating_add(width as u64).saturating_sub(col));
                let (next, glyph) = if run > 0 {
                    let next = offset + run;
                    (
                        next,
                        Span {
                            bytes: offset..next,
                            column: col,
                            width: run,
                            text: self.chunk[within..within + run as usize].to_owned(),
                            unit: Some((1, 1)),
                        },
                    )
                } else {
                    let next = self.next(offset)?.min(end);
                    (next, self.glyph(offset, next, col)?)
                };
                let merge = spans.last().is_some_and(|previous| {
                    let Some((bytes, cells)) =
                        previous.unit.filter(|unit| Some(*unit) == glyph.unit)
                    else {
                        return false;
                    };
                    // UnicodeWidthStr has cross-grapheme ligature rules (e.g.
                    // Arabic lam-alef). Joining runs must not change the map.
                    let bytes = usize::from(bytes);
                    let pair = format!(
                        "{}{}",
                        &previous.text[previous.text.len() - bytes..],
                        &glyph.text[..bytes]
                    );
                    pair.width() == usize::from(cells) * 2
                });
                let charge = if merge {
                    let previous = spans.last().expect("matching run");
                    let required = previous.text.len() + glyph.text.len();
                    if required > previous.text.capacity() {
                        // String's amortized byte allocation has an eight-byte minimum.
                        required.max(previous.text.capacity() * 2).max(8) - previous.text.capacity()
                    } else {
                        0
                    }
                } else {
                    reserve_slot(&mut spans, &mut budget, &mut lease)?;
                    glyph.text.capacity()
                };
                if charge > budget {
                    return Err(Error::ResourceLimit("visible layout budget"));
                }
                budget -= charge;
                lease.charge(charge);
                col = col.saturating_add(glyph.width);
                offset = next;
                if merge {
                    let previous = spans.last_mut().expect("matching run");
                    previous.bytes.end = glyph.bytes.end;
                    previous.width += glyph.width;
                    previous.text.push_str(&glyph.text);
                } else {
                    spans.push(glyph);
                }
            }
            // A row need not retain the Vec's spare descriptor slots. Packing
            // avoids a large terminal wasting most of its UI budget on capacity.
            let credit = (spans.capacity() - spans.len()) * std::mem::size_of::<Span>();
            let spans = spans.into_boxed_slice();
            budget += credit;
            lease.release(credit);
            reserve_slot(&mut rows, &mut budget, &mut lease)?;
            rows.push(Row { end, spans });
        }
        // If every visible line ended before the requested offset, the rows are
        // empty and remain correct at their furthest end. Clamp without scanning
        // unseen portions of long lines just to discover a global maximum width.
        if rows.iter().all(|row| row.spans.is_empty()) {
            left = left.min(furthest_column);
        }
        let cursor = (line >= top
            && line < top + height as u64
            && column >= left
            && column < left + width as u64)
            .then_some((
                (column.saturating_sub(left)) as u16,
                (line.saturating_sub(top)) as u16,
            ));
        Ok(View {
            _lease: lease,
            revision: self.snapshot.revision(),
            generation,
            width,
            height,
            top,
            left,
            rows,
            cursor,
            line,
            column,
        })
    }
}
/// Measured aggregate admission high water, including displayed and in-flight layouts.
#[cfg(test)]
pub(crate) fn layout_high_water() -> usize {
    PEAK_LAYOUT_BYTES.load(Ordering::Acquire)
}
/// One viewport request; revealing the caret is explicit so wheel scrolling stays put.
pub(crate) struct ViewRequest {
    pub position: Position,
    pub top: u64,
    pub left: u64,
    pub width: u16,
    pub height: u16,
    pub reveal: bool,
    pub generation: u64,
}
/// A bounded LRU of logical line/cell bookmarks, invalidated by content changes.
#[derive(Default)]
pub(crate) struct Checkpoints {
    revision: Option<Revision>,
    entries: VecDeque<(u64, u64, u64)>,
}
impl Checkpoints {
    /// Forget coordinates belonging to a previous content revision.
    pub fn reset(&mut self, revision: Revision) {
        if self.revision != Some(revision) {
            self.entries.clear();
            self.revision = Some(revision);
        }
    }
    /// Preserve bookmarks strictly before a committed replacement; later byte/line/cell mappings may change.
    pub fn edited(&mut self, expected: Revision, revision: Revision, start: u64) {
        // A cancelled/failed layout may never have populated this prior epoch.
        // Only bookmarks from the actual edit base can transfer to the result.
        self.reset(expected);
        self.entries.retain(|(_, byte, _)| *byte < start);
        self.revision = Some(revision);
    }
    /// Find the nearest earlier checkpoint without retaining metadata per line.
    fn best(&self, line: u64, byte: Option<u64>, cell: Option<u64>) -> Option<(u64, u64)> {
        self.entries
            .iter()
            .filter(|(l, b, c)| {
                *l == line && byte.is_none_or(|v| *b <= v) && cell.is_none_or(|v| *c <= v)
            })
            .max_by_key(|(_, b, _)| *b)
            .map(|(_, b, c)| (*b, *c))
    }
    /// Retain at most 256 sparse bookmarks.
    fn insert(&mut self, line: u64, byte: u64, column: u64) {
        if self.entries.len() == 256 {
            self.entries.pop_front();
        }
        self.entries.push_back((line, byte, column));
    }
}
/// Escape terminal controls and bidi formatting without modifying underlying bytes.
pub(crate) fn safe(text: &str) -> String {
    let mut result = String::new();
    for ch in text.chars() {
        if ch.is_control() || is_bidi_format(ch) {
            result.push_str(&format!("<U+{:04X}>", ch as u32));
        } else {
            result.push(ch);
        }
    }
    result
}

/// Direction-changing formatting that the editor displays as escaped text.
pub(crate) fn is_bidi_format(ch: char) -> bool {
    matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, Edit, OpenOptions, TextRange};
    use unicode_segmentation::UnicodeSegmentation;
    /// Build a real disk-backed snapshot without normalizing any bytes.
    fn document(text: &str) -> Document {
        let mut doc = Document::new(OpenOptions::default()).unwrap();
        doc.apply(
            doc.revision(),
            [Edit {
                range: TextRange::new(0, 0).unwrap(),
                text,
            }],
            &Cancellation::default(),
        )
        .unwrap();
        doc
    }
    /// Chunk boundaries, reverse context, CRLF and long clusters agree with a contiguous oracle.
    #[test]
    fn chunked_graphemes_match_unicode_oracle() {
        for text in [
            "e\u{301}界👩‍👩‍👧‍👦\r\n\r\t🇺🇸🇨🇦".into(),
            format!("{}{}\r\nx", "a".repeat((64 << 10) - 1), "🇺🇸🇨🇦"),
            format!("a{}b", "\u{301}".repeat(40_000)),
        ] {
            let doc = document(&text);
            let snapshot = doc.snapshot().unwrap();
            let cancel = Cancellation::default();
            let mut reader = Text::new(&snapshot, &cancel);
            let boundaries: Vec<u64> = text
                .grapheme_indices(true)
                .map(|(i, _)| i as u64)
                .chain(std::iter::once(text.len() as u64))
                .collect();
            for pair in boundaries.windows(2) {
                assert_eq!(reader.next(pair[0]).unwrap(), pair[1]);
                assert_eq!(reader.previous(pair[1]).unwrap(), pair[0]);
            }
        }
    }
    /// Tab/wide/escaped glyph hits, preferred columns and line terminators share one mapping.
    #[test]
    fn cell_mapping_and_line_bounds() {
        let doc = document("a\t界e\u{301}\x1b\r\nshort\n");
        let snapshot = doc.snapshot().unwrap();
        let cancel = Cancellation::default();
        let mut text = Text::new(&snapshot, &cancel);
        let mut cache = Checkpoints::default();
        assert_eq!(text.line_bounds(0).unwrap(), (0, 9));
        assert_eq!(text.at_column(0, 2, &mut cache).unwrap(), 1); // nearer before three-cell tab
        assert_eq!(text.at_column(0, 3, &mut cache).unwrap(), 2);
        assert_eq!(text.at_column(0, 5, &mut cache).unwrap(), 2); // wide glyph tie before
        assert_eq!(text.coordinates(5, &mut cache).unwrap(), (0, 6));
        assert_eq!(
            text.line_bounds(2).unwrap(),
            (snapshot.len(), snapshot.len())
        );
        let view = text
            .view(
                ViewRequest {
                    position: Position::default(),
                    top: 0,
                    left: 3,
                    width: 8,
                    height: 3,
                    reveal: false,
                    generation: 7,
                },
                &mut cache,
            )
            .unwrap();
        assert_eq!(view.rows.len(), 3);
        assert_eq!(view.rows[0].hit(5), 2);
        assert!(view.rows[0].spans.iter().all(|g| !g.text.contains('\x1b')));
    }
    /// A four-byte scalar immediately before LF needs five bytes of aligned tail context.
    #[test]
    fn supplementary_scalar_before_newline() {
        let doc = document("👩\n🇺\r\nlast");
        let snapshot = doc.snapshot().unwrap();
        let cancel = Cancellation::default();
        let text = Text::new(&snapshot, &cancel);
        assert_eq!(text.line_bounds(0).unwrap(), (0, 4));
        assert_eq!(text.line_bounds(1).unwrap(), (5, 9));
        assert_eq!(text.line_bounds(2).unwrap(), (11, 15));
    }
    /// A direct lookup and a later edit cannot reuse a cancelled layout's older revision.
    #[test]
    fn stale_checkpoints_cannot_survive_revision_changes() {
        let mut doc = document(&("\t".to_owned() + &"x".repeat(100_000)));
        let old = doc.snapshot().unwrap();
        let cancel = Cancellation::default();
        let mut cache = Checkpoints::default();
        Text::new(&old, &cancel)
            .at_column(0, 90_000, &mut cache)
            .unwrap();
        assert!(!cache.entries.is_empty());
        doc.apply(
            doc.revision(),
            [Edit {
                range: TextRange::new(0, 1).unwrap(),
                text: "",
            }],
            &cancel,
        )
        .unwrap();
        let intermediate = doc.snapshot().unwrap();
        assert_eq!(
            Text::new(&intermediate, &cancel)
                .at_column(0, 90_000, &mut cache)
                .unwrap(),
            90_000
        );
        // Recreate a stale cache as if that revision's layout never began.
        Text::new(&old, &cancel)
            .at_column(0, 90_000, &mut cache)
            .unwrap();
        let expected = doc.revision();
        doc.apply(
            expected,
            [Edit {
                range: TextRange::new(100_000, 100_000).unwrap(),
                text: "!",
            }],
            &cancel,
        )
        .unwrap();
        cache.edited(expected, doc.revision(), 100_000);
        let latest = doc.snapshot().unwrap();
        assert_eq!(
            Text::new(&latest, &cancel)
                .at_column(0, 90_000, &mut cache)
                .unwrap(),
            90_000
        );
    }
    /// Scroll offsets clamp at actual text and cannot overflow hit/cursor arithmetic.
    #[test]
    fn horizontal_overscroll_clamps_without_losing_clipped_glyphs() {
        let doc = document("ab界\nshort");
        let snapshot = doc.snapshot().unwrap();
        let cancel = Cancellation::default();
        for (left, expected) in [(u64::MAX, 5), (3, 3)] {
            let view = Text::new(&snapshot, &cancel)
                .view(
                    ViewRequest {
                        position: Position::default(),
                        top: 0,
                        left,
                        width: 8,
                        height: 2,
                        reveal: false,
                        generation: 1,
                    },
                    &mut Checkpoints::default(),
                )
                .unwrap();
            assert_eq!(view.left, expected);
            if left == 3 {
                assert_eq!(view.rows[0].spans[0].text, "界");
                assert_eq!(view.rows[0].hit(3), 2);
            }
        }
    }
    /// A read window inside one long line does not prevent later document lines filling the viewport.
    #[test]
    fn viewport_skips_the_offscreen_remainder_of_a_long_line() {
        let doc = document(&format!("{}\nnext\nlast", "x".repeat(1 << 20)));
        let snapshot = doc.snapshot().unwrap();
        let cancel = Cancellation::default();
        let mut cache = Checkpoints::default();
        let view = Text::new(&snapshot, &cancel)
            .view(
                ViewRequest {
                    position: Position::default(),
                    top: 0,
                    left: 0,
                    width: 40,
                    height: 3,
                    reveal: false,
                    generation: 1,
                },
                &mut cache,
            )
            .unwrap();
        assert_eq!(view.rows.len(), 3);
        assert_eq!(view.rows[0].spans.iter().map(|g| g.width).sum::<u64>(), 40);
        assert_eq!(
            view.rows[1]
                .spans
                .iter()
                .map(|g| g.text.as_str())
                .collect::<String>(),
            "next"
        );
        assert_eq!(
            view.rows[2]
                .spans
                .iter()
                .map(|g| g.text.as_str())
                .collect::<String>(),
            "last"
        );
    }

    /// A large ordinary terminal retains spans per row, rather than a record per ASCII character.
    #[test]
    fn large_viewport_uses_compact_ascii_spans() {
        let body = ("x".repeat(240) + "\n").repeat(80);
        let doc = document(&body);
        let snapshot = doc.snapshot().unwrap();
        let cancel = Cancellation::default();
        let view = Text::new(&snapshot, &cancel)
            .view(
                ViewRequest {
                    position: Position::default(),
                    top: 0,
                    left: 0,
                    width: 240,
                    height: 80,
                    reveal: false,
                    generation: 1,
                },
                &mut Checkpoints::default(),
            )
            .unwrap();
        assert_eq!(view.rows.len(), 80);
        assert!(
            view.rows
                .iter()
                .all(|row| row.spans.len() == 1 && row.spans[0].width == 240)
        );
        assert_eq!(view.rows[40].hit(239), 40 * 241 + 239);
    }
    /// Large Unicode views retain runs, and every visible cell agrees with navigation.
    #[test]
    fn compact_unicode_runs_preserve_cell_mapping() {
        for unit in ["é", "e\u{301}", "界", "👩‍👧"] {
            let body = (unit.repeat(300) + "\n").repeat(100);
            let doc = document(&body);
            let snapshot = doc.snapshot().unwrap();
            let cancel = Cancellation::default();
            let mut text = Text::new(&snapshot, &cancel);
            let mut cache = Checkpoints::default();
            let view = text
                .view(
                    ViewRequest {
                        position: Position::default(),
                        top: 0,
                        left: 1,
                        width: 300,
                        height: 100,
                        reveal: false,
                        generation: 1,
                    },
                    &mut cache,
                )
                .unwrap();
            assert!(view.rows.iter().all(|row| row.spans.len() == 1));
            for column in 1..301 {
                assert_eq!(
                    view.rows[0].hit(column),
                    text.at_column(0, column, &mut cache).unwrap(),
                    "{unit:?} column {column}"
                );
            }
        }
        let doc = document("لا");
        let snapshot = doc.snapshot().unwrap();
        let cancel = Cancellation::default();
        let view = Text::new(&snapshot, &cancel)
            .view(
                ViewRequest {
                    position: Position::default(),
                    top: 0,
                    left: 0,
                    width: 40,
                    height: 1,
                    reveal: false,
                    generation: 1,
                },
                &mut Checkpoints::default(),
            )
            .unwrap();
        assert_eq!(
            view.rows[0].spans.len(),
            2,
            "cross-grapheme ligature must not collapse cell widths"
        );
        assert!(layout_high_water() <= 6 << 20);
    }
    /// Count real retained capacities independently, including tiny per-row vectors and mixed strides.
    #[test]
    fn retained_layout_allocations_are_fully_charged() {
        for (body, width, height) in [
            (("aé".repeat(150) + "\n").repeat(100), 300, 100),
            ("x\n".repeat(4096), 1, 4096),
        ] {
            let doc = document(&body);
            let snapshot = doc.snapshot().unwrap();
            let cancel = Cancellation::default();
            let view = Text::new(&snapshot, &cancel)
                .view(
                    ViewRequest {
                        position: Position::default(),
                        top: 0,
                        left: 0,
                        width,
                        height,
                        reveal: false,
                        generation: 1,
                    },
                    &mut Checkpoints::default(),
                )
                .unwrap();
            assert_eq!(view.rows.len(), height as usize);
            let retained = view.rows.capacity() * std::mem::size_of::<Row>()
                + view
                    .rows
                    .iter()
                    .map(|row| {
                        row.spans.len() * std::mem::size_of::<Span>()
                            + row
                                .spans
                                .iter()
                                .map(|span| span.text.capacity())
                                .sum::<usize>()
                    })
                    .sum::<usize>();
            assert!(
                retained <= view._lease.charged,
                "{retained} > {}",
                view._lease.charged
            );
            assert!(view._lease.charged <= 3 << 20);
        }
    }

    /// Huge graphemes remain navigable and are represented by one bounded labeled glyph.
    #[test]
    fn huge_cluster_placeholder_and_cancellation() {
        let doc = document(&format!("a{}\nnext", "\u{301}".repeat(40_000)));
        let snapshot = doc.snapshot().unwrap();
        let cancel = Cancellation::default();
        let mut cache = Checkpoints::default();
        let view = Text::new(&snapshot, &cancel)
            .view(
                ViewRequest {
                    position: Position::default(),
                    top: 0,
                    left: 0,
                    width: 40,
                    height: 4,
                    reveal: false,
                    generation: 1,
                },
                &mut cache,
            )
            .unwrap();
        assert_eq!(view.rows[0].spans.len(), 1);
        assert_eq!(view.rows[0].spans[0].text, "[cluster]");
        assert!(view.rows[0].spans[0].bytes.end > 64 << 10);
        cancel.cancel();
        assert!(matches!(
            Text::new(&snapshot, &cancel).next(0),
            Err(Error::Cancelled)
        ));
    }
}
