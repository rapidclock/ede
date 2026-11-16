//! Reference resolution for one Markdown block tree and its inline children.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range as ByteRange,
    time::Instant,
};
use tree_sitter::{Node, Range, Tree};
use unicase::UniCase;

use super::{EngineResult, check, child, intersect, slices, without_children};

/// Normalized definitions from the current Markdown document, including quoted ones.
#[derive(Default)]
pub(super) struct References(BTreeSet<String>);

impl References {
    /// Record the label of a structurally valid block definition.
    pub(super) fn add(&mut self, definition: Node<'_>, source: &[u8], ranges: &[Range]) {
        if let Some(label) =
            child(definition, "link_label").and_then(|node| normalized_label(node, source, ranges))
        {
            self.0.insert(label);
        }
    }

    /// Apply definition-aware, left-to-right association to the grammar's adjacent labels.
    pub(super) fn resolve_inline(
        &self,
        tree: &Tree,
        source: &[u8],
        ranges: &[Range],
        deadline: Instant,
    ) -> EngineResult<ResolvedLinks> {
        let mut resolved = ResolvedLinks::default();
        if self.0.is_empty() {
            return Ok(resolved);
        }
        let mut cursor = tree.walk();
        let mut parts = Vec::new();
        loop {
            check(deadline)?;
            let node = cursor.node();
            if is_reference(node)
                && node.prev_named_sibling().is_none_or(|previous| {
                    previous.end_byte() != node.start_byte() || !is_reference(previous)
                })
            {
                parts.clear();
                let mut tail = node;
                let following = loop {
                    check(deadline)?;
                    parts.extend(parts_of(tail));
                    let next = tail
                        .next_named_sibling()
                        .filter(|next| next.start_byte() == tail.end_byte());
                    match next {
                        Some(next) if is_reference(next) => tail = next,
                        _ => break next,
                    }
                };
                if let Some(part) = following
                    .filter(|node| node.kind() == "inline_link")
                    .and_then(|node| parts_of(node).next())
                {
                    parts.push(part);
                }
                let mut index = 0;
                while let Some(part) = parts.get(index) {
                    check(deadline)?;
                    if part.is_inline_link() {
                        break;
                    }
                    let next = parts
                        .get(index + 1)
                        .filter(|next| part.bytes.end == next.bytes.start && !next.image);
                    let target = next.and_then(|next| next.name(source, ranges));
                    let collapsed = next.is_some_and(|next| next.empty);
                    if let Some(next) = next.filter(|_| collapsed || target.is_some()) {
                        let label = if collapsed {
                            part.name(source, ranges)
                        } else {
                            target
                        };
                        if label.is_some_and(|name| self.0.contains(&name)) {
                            resolved.record(part);
                            resolved.record(next);
                            index += 2;
                            continue;
                        }
                    } else if part
                        .name(source, ranges)
                        .is_some_and(|name| self.0.contains(&name))
                    {
                        resolved.record(part);
                    }
                    index += 1;
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return Ok(resolved);
                }
            }
        }
    }
}

/// Bracket spans recognized as reference syntax, indexed by their original Tree-sitter owner.
#[derive(Default)]
pub(super) struct ResolvedLinks {
    references: BTreeSet<(usize, usize, usize)>,
    consumed_inline_links: BTreeMap<usize, usize>,
}

impl ResolvedLinks {
    fn record(&mut self, part: &Part<'_>) {
        if part.is_inline_link() {
            self.consumed_inline_links
                .insert(part.owner.id(), part.bytes.end);
        } else {
            self.references
                .insert((part.owner.id(), part.bytes.start, part.bytes.end));
        }
    }

    /// Suppress syntax coloring for unresolved references without suppressing nested inline styling.
    pub(super) fn styles(&self, capture: Node<'_>, reference_style: bool) -> bool {
        let Some(mut owner) = capture.parent() else {
            return true;
        };
        if owner.kind() == "link_label" {
            let Some(parent) = owner.parent() else {
                return true;
            };
            owner = parent;
        }
        let id = owner.id();
        if let Some(bracket_end) = self.consumed_inline_links.get(&id) {
            return capture.end_byte() <= *bracket_end;
        }
        if !reference_style || !is_reference(owner) {
            return true;
        }
        self.references
            .range((id, 0, 0)..=(id, capture.start_byte(), usize::MAX))
            .next_back()
            .is_some_and(|(_, _, end)| *end >= capture.end_byte())
    }
}

/// A single source bracket pair, independent of which neighboring label the parser attached.
struct Part<'tree> {
    owner: Node<'tree>,
    bytes: ByteRange<usize>,
    text: Option<Node<'tree>>,
    empty: bool,
    image: bool,
}

impl Part<'_> {
    fn is_inline_link(&self) -> bool {
        self.owner.kind() == "inline_link"
    }

    fn name(&self, source: &[u8], ranges: &[Range]) -> Option<String> {
        self.text
            .and_then(|node| normalized_label(node, source, ranges))
    }
}

/// Link references and images without an inline destination can participate in a label run.
fn is_reference(node: Node<'_>) -> bool {
    match node.kind() {
        "full_reference_link" | "collapsed_reference_link" | "shortcut_link" => true,
        "image" => !node
            .children(&mut node.walk())
            .any(|child| child.kind() == "("),
        _ => false,
    }
}

/// Split a reference or an inline link into its direct source bracket pairs.
fn parts_of(node: Node<'_>) -> impl Iterator<Item = Part<'_>> {
    let mut cursor = node.walk();
    let mut brackets = node
        .children(&mut cursor)
        .filter(|child| matches!(child.kind(), "[" | "]"));
    let image = node.kind() == "image";
    let first = brackets
        .next()
        .zip(brackets.next())
        .map(|(open, close)| Part {
            owner: node,
            bytes: node.start_byte()..close.end_byte(),
            text: child(
                node,
                if image {
                    "image_description"
                } else {
                    "link_text"
                },
            ),
            empty: open.end_byte() == close.start_byte(),
            image,
        });
    let second = if let Some(label) = child(node, "link_label") {
        Some(Part {
            owner: node,
            bytes: label.byte_range(),
            text: Some(label),
            empty: false,
            image: false,
        })
    } else {
        brackets
            .next()
            .zip(brackets.next())
            .map(|(open, close)| Part {
                owner: node,
                bytes: open.start_byte()..close.end_byte(),
                text: None,
                empty: open.end_byte() == close.start_byte(),
                image: false,
            })
    };
    first.into_iter().chain(second)
}

/// Recover HTML in list error nodes whose only named children are container prefixes.
pub(super) fn is_list_html(node: Node<'_>, source: &[u8], ranges: &[Range]) -> bool {
    let inside_list = node.parent().is_some_and(|parent| {
        parent.kind() == "list_item"
            || (parent.is_error()
                && node
                    .prev_named_sibling()
                    .is_some_and(|previous| previous.kind().starts_with("list_marker_")))
    });
    if !inside_list
        || node.named_child_count() == 0
        || node
            .named_children(&mut node.walk())
            .any(|child| child.kind() != "block_continuation")
    {
        return false;
    }
    slices(node.range(), ranges, source)
        .next()
        .and_then(|prefix| prefix.trim_ascii_start().strip_prefix(b"<"))
        .and_then(|opening| opening.first())
        .is_some_and(u8::is_ascii_alphabetic)
}

/// Read logical label bytes, excluding quote/list prefixes and gaps in embedded ranges.
fn normalized_label(node: Node<'_>, source: &[u8], ranges: &[Range]) -> Option<String> {
    // A label contains at most 999 Unicode characters, excluding its outer brackets.
    const CHAR_LIMIT: usize = 999;
    const BYTE_LIMIT: usize = CHAR_LIMIT * 4 + 2;
    let mut bytes = Vec::new();
    for part in without_children(node, |child| child.kind() == "block_continuation")
        .into_iter()
        .flat_map(|range| intersect(range, ranges))
    {
        let data = source.get(part.start_byte..part.end_byte)?;
        if bytes.len() + data.len() > BYTE_LIMIT {
            return None;
        }
        bytes.extend_from_slice(data);
    }
    let mut label = std::str::from_utf8(&bytes).ok()?;
    if node.kind() == "link_label" {
        label = label.strip_prefix('[')?.strip_suffix(']')?;
    }
    let mut escaped = false;
    let mut count = 0;
    let mut was_cr = false;
    for scalar in label.chars() {
        count += usize::from(!(was_cr && scalar == '\n'));
        if count > CHAR_LIMIT || (!escaped && matches!(scalar, '[' | ']')) {
            return None;
        }
        escaped = scalar == '\\' && !escaped;
        was_cr = scalar == '\r';
    }
    let mut words = label
        .split([' ', '\t', '\r', '\n'])
        .filter(|word| !word.is_empty());
    let mut normalized = words.next()?.to_owned();
    for word in words {
        normalized.push(' ');
        normalized.push_str(word);
    }
    Some(UniCase::new(normalized).to_folded_case())
}

/// An unclosed HTML comment uses the Markdown block range when the HTML grammar cannot color it.
pub(super) fn is_unclosed_html_comment(node: Node<'_>, source: &[u8], ranges: &[Range]) -> bool {
    let mut parts = slices(node.range(), ranges, source);
    let Some(opening) = parts
        .next()
        .and_then(|prefix| prefix.trim_ascii_start().strip_prefix(b"<!--"))
    else {
        return false;
    };
    !std::iter::once(opening)
        .chain(parts)
        .any(|part| memchr::memmem::find(part, b"-->").is_some())
}
