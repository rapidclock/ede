//! Viewport identities and bounded logical glyph descriptions for the worker.

use crate::{
    document::{DocumentId, Permit, Revision},
    editor::TabId,
    text::View,
};

use super::Category;

/// Separate capacity for document text and normalized viewport replies.
pub(super) const MAX_RANGES: usize = 16_384;

/// Identifies the exact document, revision and visible geometry used for a projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ViewKey {
    tab: TabId,
    document: DocumentId,
    pub(super) revision: Revision,
    generation: u64,
    top: u64,
    left: u64,
    width: u16,
    height: u16,
}

impl ViewKey {
    /// Capture the identity without retaining the viewport allocation.
    pub(crate) fn new(tab: TabId, document: DocumentId, view: &View) -> Self {
        Self {
            tab,
            document,
            revision: view.revision,
            generation: view.generation,
            top: view.top,
            left: view.left,
            width: view.width,
            height: view.height,
        }
    }
}

/// Contiguous glyphs of the same byte width. An atomic glyph has a count of one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GlyphRun {
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) stride: u32,
}

impl GlyphRun {
    /// Encode visible text only, retaining the full bytes of clipped end glyphs.
    pub(super) fn collect(view: &View) -> Option<Vec<Self>> {
        let mut runs: Vec<Self> = Vec::new();
        let right = view.left + u64::from(view.width);
        for span in view.rows.iter().flat_map(|row| row.spans.iter()) {
            let lo = span.column.max(view.left);
            let hi = (span.column + span.width).min(right);
            if lo >= hi {
                continue;
            }
            let (start, end, stride) = match span.unit {
                Some((bytes, cells)) => {
                    let (bytes, cells) = (u64::from(bytes), u64::from(cells));
                    (
                        span.bytes.start + (lo - span.column) / cells * bytes,
                        span.bytes.start + (hi - span.column).div_ceil(cells) * bytes,
                        bytes,
                    )
                }
                None => (
                    span.bytes.start,
                    span.bytes.end,
                    span.bytes.end - span.bytes.start,
                ),
            };
            if stride == 0 {
                continue;
            }
            let run = Self {
                start: start.try_into().ok()?,
                end: end.try_into().ok()?,
                stride: stride.try_into().ok()?,
            };
            if let Some(last) = runs.last_mut()
                && last.end == run.start
                && last.stride == run.stride
            {
                last.end = run.end;
                continue;
            }
            if runs.len() == MAX_RANGES {
                return None;
            }
            runs.push(run);
        }
        Some(runs)
    }
}

/// A foreground category spanning one or more complete adjacent visible glyphs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Styled {
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) category: Category,
}

/// Immutable spans with a shared storage buffer charge lasting through publication.
pub(crate) struct Projection {
    pub(crate) key: ViewKey,
    pub(crate) ranges: Vec<Styled>,
    pub(super) _permit: Permit,
}

/// Reject worker output that crosses an unmapped gap or splits a grapheme.
pub(super) fn valid(runs: &[GlyphRun], spans: &[Styled]) -> bool {
    let mut index = 0;
    let mut previous = 0;
    for span in spans {
        if span.start >= span.end || span.start < previous {
            return false;
        }
        let mut at = span.start;
        while at < span.end {
            while runs.get(index).is_some_and(|run| run.end <= at) {
                index += 1;
            }
            let Some(run) = runs.get(index) else {
                return false;
            };
            if at < run.start || !(at - run.start).is_multiple_of(run.stride) {
                return false;
            }
            at = span.end.min(run.end);
            if !(at - run.start).is_multiple_of(run.stride) {
                return false;
            }
        }
        previous = span.end;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_ranges_cannot_split_glyphs_or_bridge_unmapped_text() {
        let runs = [
            GlyphRun {
                start: 2,
                end: 8,
                stride: 3,
            },
            GlyphRun {
                start: 8,
                end: 10,
                stride: 2,
            },
            GlyphRun {
                start: 30,
                end: 36,
                stride: 6,
            },
        ];
        let style = |start, end| Styled {
            start,
            end,
            category: Category::Keyword,
        };
        assert!(valid(&runs, &[style(2, 10), style(30, 36)]));
        assert!(!valid(&runs, &[style(3, 8)]));
        assert!(!valid(&runs, &[style(5, 7)]));
        assert!(!valid(&runs, &[style(8, 36)]));
        assert!(!valid(&runs, &[style(5, 8), style(2, 5)]));
    }
}
