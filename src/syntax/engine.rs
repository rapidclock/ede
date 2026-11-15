//! Tree-sitter definitions, bounded parsing and syntax projection on original bytes.

use std::{collections::BTreeMap, ops::ControlFlow, time::Instant};
use tree_sitter::{
    InputEdit, Language as Grammar, Node, ParseOptions, Parser, Point, Query, QueryCursor,
    QueryCursorOptions, Range, StreamingIterator, Tree,
};

use super::{
    Category, Fallback, Language,
    config::Rules,
    projection::{GlyphRun, MAX_RANGES, Styled},
};

/// Markdown has two grammars. Its custom query uses the block grammar.
pub(super) const INLINE: &str = "markdown-inline";
const MAX_DEPTH: u8 = 8;

type EngineResult<T> = Result<T, Fallback>;

/// Retained state belongs to one active document revision in one process.
pub(super) struct Analysis {
    source: String,
    layers: Vec<Layer>,
    neutral: Vec<(Range, u8, Option<Category>)>,
    queries: BTreeMap<&'static str, Compiled>,
}

struct Layer {
    grammar: &'static str,
    tree: Tree,
    depth: u8,
    ranges: Vec<Range>,
}

struct Compiled {
    built_in: Query,
    custom: Option<Query>,
}

impl Analysis {
    /// Build the root and nested trees before publishing any projection.
    pub(super) fn new(
        mut source: String,
        language: &'static Language,
        rules: &Rules,
        deadline: Instant,
    ) -> EngineResult<Self> {
        if language.id == "ini" && !source.ends_with('\n') {
            // The upstream setting and comment productions require a terminator.
            // This synthetic LF is outside every projected offset.
            source.push('\n');
        }
        let mut engine = Self {
            source,
            layers: Vec::new(),
            neutral: Vec::new(),
            queries: BTreeMap::new(),
        };
        engine.add(language.id, &[], 0, rules, &mut Parser::new(), deadline)?;
        Ok(engine)
    }

    /// Select the category covering the first scalar of each visible glyph.
    pub(super) fn project(
        &self,
        runs: &[GlyphRun],
        deadline: Instant,
    ) -> EngineResult<Vec<Styled>> {
        let mut glyphs = Vec::new();
        for run in runs {
            let mut start = run.start;
            while start < run.end {
                check(deadline)?;
                glyphs.push(Glyph {
                    start,
                    end: start + run.stride,
                    style: None,
                });
                start += run.stride;
            }
        }
        if glyphs.is_empty() {
            return Ok(Vec::new());
        }
        for (range, depth, category) in &self.neutral {
            coat(
                &mut glyphs,
                range.start_byte,
                range.end_byte,
                *category,
                (*depth, false, 0, false, 0),
                deadline,
            )?;
        }
        let visible = glyphs[0].start as usize..glyphs.last().unwrap().end as usize;
        for layer in &self.layers {
            check(deadline)?;
            let root = layer.tree.root_node();
            if root.end_byte() <= visible.start || root.start_byte() >= visible.end {
                continue;
            }
            let compiled = &self.queries[layer.grammar];
            for (query, custom) in [
                (Some(&compiled.built_in), false),
                (compiled.custom.as_ref(), true),
            ] {
                let Some(query) = query else { continue };
                let names = query.capture_names();
                let mut cursor = QueryCursor::new();
                cursor.set_byte_range(visible.clone());
                cursor.set_match_limit(4096);
                let mut progress = |_: &tree_sitter::QueryCursorState| progress(deadline);
                let options = QueryCursorOptions::new().progress_callback(&mut progress);
                {
                    let text = |node: Node<'_>| {
                        slices(node.range(), &layer.ranges, self.source.as_bytes())
                    };
                    let mut matches = cursor.matches_with_options(query, root, text, options);
                    while let Some(found) = matches.next() {
                        check(deadline)?;
                        for capture in found.captures() {
                            let name = names[capture.index as usize];
                            let category = if custom {
                                Category::named(name)
                            } else {
                                builtin_category(name)
                            };
                            if !custom
                                && layer.grammar == "ini"
                                && category == Some(Category::Comment)
                                && capture.node.start_position().column != 0
                            {
                                continue;
                            }
                            if category.is_none() {
                                continue;
                            }
                            let range = capture.node.byte_range();
                            let priority = if custom {
                                (layer.depth, true, found.pattern_index, false, 0)
                            } else {
                                (
                                    layer.depth,
                                    false,
                                    usize::MAX - range.len(),
                                    category == Some(Category::Property),
                                    usize::MAX - found.pattern_index,
                                )
                            };
                            let pieces = if !custom
                                && category == Some(Category::String)
                                && matches!(capture.node.kind(), "template_string" | "string")
                                && matches!(layer.grammar, "javascript" | "typescript" | "python")
                            {
                                without_children(capture.node, |node| {
                                    matches!(node.kind(), "template_substitution" | "interpolation")
                                })
                            } else {
                                vec![capture.node.range()]
                            };
                            for range in pieces
                                .into_iter()
                                .flat_map(|piece| intersect(piece, &layer.ranges))
                            {
                                coat(
                                    &mut glyphs,
                                    range.start_byte,
                                    range.end_byte,
                                    category,
                                    priority,
                                    deadline,
                                )?;
                            }
                        }
                    }
                }
                check(deadline)?;
                if cursor.did_exceed_match_limit() {
                    return Err(Fallback::Output);
                }
            }
        }
        let mut styled: Vec<Styled> = Vec::new();
        for glyph in glyphs {
            check(deadline)?;
            if let Some((_, Some(category))) = glyph.style {
                if let Some(last) = styled.last_mut()
                    && last.end == glyph.start
                    && last.category == category
                {
                    last.end = glyph.end;
                    continue;
                }
                if styled.len() == MAX_RANGES {
                    return Err(Fallback::Output);
                }
                styled.push(Styled {
                    start: glyph.start,
                    end: glyph.end,
                    category,
                });
            }
        }
        Ok(styled)
    }

    /// Parse one layer and discover grammar-specific child ranges without recursive AST walking.
    fn add(
        &mut self,
        id: &'static str,
        ranges: &[Range],
        depth: u8,
        rules: &Rules,
        parser: &mut Parser,
        deadline: Instant,
    ) -> EngineResult<()> {
        if depth > MAX_DEPTH {
            return Err(Fallback::Output);
        }
        check(deadline)?;
        let (grammar, source) = grammar(id);
        if !self.queries.contains_key(id) {
            let extra = match id {
                "json" => r#"["{" "}" "[" "]" ":" ","] @punctuation.delimiter"#,
                "css" => "(declaration (plain_value) @constant)",
                _ => "",
            };
            let built_in = if extra.is_empty() {
                Query::new(&grammar, source)
            } else {
                Query::new(&grammar, &format!("{source}\n{extra}"))
            }
            .map_err(|_| Fallback::Worker)?;
            let custom = rules
                .queries
                .get(id)
                .map(|text| custom_query(&grammar, text))
                .transpose()
                .map_err(|_| Fallback::Worker)?;
            self.queries.insert(id, Compiled { built_in, custom });
        }

        // Frontmatter only applies to the first logical line of a Markdown layer.
        let mut adjusted = Vec::new();
        let front = if id == "markdown" {
            frontmatter(&self.source, ranges, deadline)?
        } else {
            None
        };
        if let Some((opening_end, closing_start, ending)) = front {
            let bytes = &self.source;
            let start = ranges.first().map_or(0, |range| range.start_byte);
            let end = ranges.last().map_or(bytes.len(), |range| range.end_byte);
            for delimiter in [
                byte_range(bytes, start, opening_end),
                byte_range(bytes, closing_start, ending),
            ] {
                self.neutral.extend(
                    intersect(delimiter, ranges)
                        .map(|range| (range, depth, Some(Category::Punctuation))),
                );
            }
            let yaml: Vec<_> = intersect(byte_range(bytes, opening_end, closing_start), ranges)
                .filter(|range| range.start_byte < range.end_byte)
                .collect();
            if ending < end {
                adjusted.extend(intersect(byte_range(bytes, ending, end), ranges));
            }
            if !yaml.is_empty() {
                self.add("yaml", &yaml, depth + 1, rules, parser, deadline)?;
            }
            if adjusted.is_empty() {
                return Ok(());
            }
        }
        let ranges = if front.is_some() { &adjusted } else { ranges };
        parser
            .set_language(&grammar)
            .map_err(|_| Fallback::Worker)?;
        let data = self.source.as_bytes();
        let tree = parse(parser, data, ranges, id == "markdown", deadline)?;
        check(deadline)?;

        let mut injections = Vec::new();
        if matches!(id, "markdown" | "html") {
            let mut cursor = tree.walk();
            loop {
                check(deadline)?;
                let node = cursor.node();
                let skip_children = if id == "markdown" {
                    match node.kind() {
                        "inline" | "pipe_table_cell" => {
                            injections.push((
                                Some(INLINE),
                                without_children(node, |node| node.kind() == "block_continuation"),
                            ));
                            true
                        }
                        "fenced_code_block" => {
                            let info = child(node, "info_string")
                                .and_then(|info| self.source.get(info.byte_range()));
                            let language = info
                                .and_then(|info| info.split_whitespace().next())
                                .and_then(|alias| rules.fence(alias));
                            if let Some(body) = child(node, "code_fence_content") {
                                injections.push((
                                    language.map(|language| language.id),
                                    without_children(body, |node| {
                                        node.kind() == "block_continuation"
                                    }),
                                ));
                            }
                            true
                        }
                        _ => false,
                    }
                } else {
                    match node.kind() {
                        "script_element" | "style_element" => {
                            if let Some(body) = child(node, "raw_text") {
                                injections.push((
                                    html_language(node, &self.source, ranges),
                                    vec![body.range()],
                                ));
                            }
                            true
                        }
                        _ => false,
                    }
                };
                if !skip_children && cursor.goto_first_child() {
                    continue;
                }
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() {
                        break;
                    }
                }
                if cursor.depth() == 0 {
                    break;
                }
            }
        }
        self.layers.push(Layer {
            grammar: id,
            tree,
            depth,
            ranges: ranges.to_vec(),
        });
        for (injection, child_ranges) in injections {
            let child_ranges: Vec<_> = child_ranges
                .into_iter()
                .flat_map(|range| intersect(range, ranges))
                .collect();
            if child_ranges.is_empty() {
                continue;
            }
            match injection {
                Some(id) => {
                    if id != INLINE {
                        // Unclassified child text inherits terminal foreground instead
                        // of the surrounding parent's markup or code capture.
                        self.neutral
                            .extend(child_ranges.iter().map(|range| (*range, depth + 1, None)));
                    }
                    self.add(id, &child_ranges, depth + 1, rules, parser, deadline)?;
                }
                None => self.neutral.extend(
                    child_ranges
                        .into_iter()
                        .map(|range| (range, depth + 1, Some(Category::Code))),
                ),
            }
        }
        Ok(())
    }
}

type Priority = (u8, bool, usize, bool, usize);

struct Glyph {
    start: u32,
    end: u32,
    style: Option<(Priority, Option<Category>)>,
}

/// Phase ordering gives nested layers priority, then user rules, then built-in specificity.
fn coat(
    glyphs: &mut [Glyph],
    start: usize,
    end: usize,
    category: Option<Category>,
    priority: Priority,
    deadline: Instant,
) -> EngineResult<()> {
    let first = glyphs.partition_point(|glyph| (glyph.start as usize) < start);
    let last = glyphs.partition_point(|glyph| (glyph.start as usize) < end);
    for glyph in &mut glyphs[first..last] {
        check(deadline)?;
        if glyph.style.is_none_or(|(old, _)| priority >= old) {
            glyph.style = Some((priority, category));
        }
    }
    Ok(())
}

/// Reusable checked nested range without quote/list continuation bytes.
fn without_children(node: Node<'_>, omit: impl FnMut(&Node<'_>) -> bool) -> Vec<Range> {
    let mut result = Vec::new();
    let mut start = node.start_byte();
    let mut point = node.start_position();
    let mut cursor = node.walk();
    for prefix in node.named_children(&mut cursor).filter(omit) {
        if start < prefix.start_byte() {
            result.push(Range {
                start_byte: start,
                end_byte: prefix.start_byte(),
                start_point: point,
                end_point: prefix.start_position(),
            });
        }
        start = prefix.end_byte();
        point = prefix.end_position();
    }
    if start < node.end_byte() {
        result.push(Range {
            start_byte: start,
            end_byte: node.end_byte(),
            start_point: point,
            end_point: node.end_position(),
        });
    }
    result
}

/// Match a direct named child while retaining absolute positions.
fn child<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|node| node.kind() == kind)
}

/// Decide whether a script or style body contains a supported type.
fn html_language(node: Node<'_>, source: &str, ranges: &[Range]) -> Option<&'static str> {
    let script = node.kind() == "script_element";
    if let Some(tag) = child(node, "start_tag") {
        let mut cursor = tag.walk();
        for attribute in tag
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "attribute")
        {
            let Some(name) = child(attribute, "attribute_name") else {
                continue;
            };
            if !source[name.byte_range()].eq_ignore_ascii_case("type") {
                continue;
            }
            let value = child(attribute, "attribute_value").or_else(|| {
                child(attribute, "quoted_attribute_value")
                    .and_then(|node| child(node, "attribute_value"))
            });
            let value: String = value
                .into_iter()
                .flat_map(|node| intersect(node.range(), ranges))
                .map(|range| &source[range.start_byte..range.end_byte])
                .collect();
            let value = value.trim_matches([' ', '\t', '\n', '\r', '\x0c']);
            let accepted = if script {
                ["", "module", "text/javascript", "application/javascript"]
                    .iter()
                    .any(|known| value.eq_ignore_ascii_case(known))
            } else {
                value.is_empty() || value.eq_ignore_ascii_case("text/css")
            };
            if !accepted {
                return None;
            }
            break;
        }
    }
    Some(if script { "javascript" } else { "css" })
}

/// Recognize exact delimiters in the logical concatenation of the layer's ranges.
fn frontmatter(
    source: &str,
    ranges: &[Range],
    deadline: Instant,
) -> EngineResult<Option<(usize, usize, usize)>> {
    let full = ranges.is_empty().then_some((0, source.len()));
    let bounds = full
        .into_iter()
        .chain(ranges.iter().map(|r| (r.start_byte, r.end_byte)));
    let mut opening = None;
    let mut first = None;
    let mut last = 0;
    let mut length = 0;
    let mut prefix = [0u8; 5];
    for (start, end) in bounds {
        let mut at = start;
        for piece in source.as_bytes()[start..end].split_inclusive(|&byte| byte == b'\n') {
            check(deadline)?;
            first.get_or_insert(at);
            let available = prefix.len() - length.min(prefix.len());
            let copied = available.min(piece.len());
            if copied != 0 {
                prefix[length..length + copied].copy_from_slice(&piece[..copied]);
            }
            length += piece.len();
            at += piece.len();
            last = at;
            if piece.ends_with(b"\n") {
                let delimiter =
                    length <= prefix.len() && matches!(&prefix[..length], b"---\n" | b"---\r\n");
                if let Some(opening_end) = opening {
                    if delimiter {
                        return Ok(Some((opening_end, first.unwrap(), at)));
                    }
                } else if delimiter {
                    opening = Some(at);
                } else {
                    return Ok(None);
                }
                first = None;
                length = 0;
            }
        }
    }
    Ok(opening
        .zip(first)
        .filter(|_| length == 3 && &prefix[..3] == b"---")
        .map(|(opening_end, closing_start)| (opening_end, closing_start, last)))
}

/// Convert logical byte offsets to original Tree-sitter byte-column points.
fn byte_range(source: &str, start: usize, end: usize) -> Range {
    let point = |offset| {
        let before = &source.as_bytes()[..offset];
        Point::new(
            memchr::memchr_iter(b'\n', before).count(),
            offset - memchr::memrchr(b'\n', before).map_or(0, |lf| lf + 1),
        )
    };
    Range {
        start_byte: start,
        end_byte: end,
        start_point: point(start),
        end_point: point(end),
    }
}

/// Compile recognized predicates and canonical categories. General predicates are
/// not evaluated by Tree-sitter's Rust cursor, so accepting them would misstyle text.
fn custom_query(grammar: &Grammar, text: &str) -> Result<Query, String> {
    let query = Query::new(grammar, text)
        .map_err(|error| format!("line {}: {}", error.row + 1, error.message))?;
    for name in query.capture_names() {
        if Category::named(name).is_none() {
            return Err(format!("unknown capture @{name}"));
        }
    }
    for index in 0..query.pattern_count() {
        if !query.general_predicates(index).is_empty()
            || !query.property_predicates(index).is_empty()
            || !query.property_settings(index).is_empty()
        {
            return Err("unsupported query predicate or directive".into());
        }
    }
    Ok(query)
}

/// Validate every candidate override in the isolated worker before publication.
pub(super) fn validate(rules: &Rules, deadline: Instant) -> Result<(), (&'static str, String)> {
    for (id, text) in &rules.queries {
        if Instant::now() >= deadline {
            return Err((id, Fallback::Timeout.message().into()));
        }
        custom_query(&grammar(id).0, text).map_err(|error| (*id, error))?;
    }
    Ok(())
}

fn progress(deadline: Instant) -> ControlFlow<()> {
    if Instant::now() >= deadline {
        ControlFlow::Break(())
    } else {
        ControlFlow::Continue(())
    }
}

fn check(deadline: Instant) -> EngineResult<()> {
    if progress(deadline).is_break() {
        Err(Fallback::Timeout)
    } else {
        Ok(())
    }
}

/// Normalize released query names without importing engine-specific themes.
fn builtin_category(name: &str) -> Option<Category> {
    use Category::*;
    Some(match name {
        "text.literal" => Code,
        "string.special.key" => Property,
        _ => match name.split('.').next()? {
            "comment" => Comment,
            "string" | "character" | "escape" => String,
            "number" | "float" => Number,
            "keyword" => Keyword,
            "type" | "constructor" => Type,
            "property" | "field" => Property,
            "text" | "markup" | "label" => Markup,
            "function" | "method" => Function,
            "constant" | "boolean" => Constant,
            "operator" => Operator,
            "punctuation" => Punctuation,
            "variable" if name != "variable" => Variable,
            "tag" => Tag,
            "attribute" => Attribute,
            _ => return None,
        },
    })
}

/// Published generated grammars and their bundled plus ede-specific initial queries.
fn grammar(id: &str) -> (Grammar, &'static str) {
    match id {
        "markdown" => (
            tree_sitter_md::LANGUAGE.into(),
            concat!(
                include_str!("queries/markdown.scm"),
                "\n",
                "(task_list_marker_checked) @text.reference\n(task_list_marker_unchecked) @text.reference\n(pipe_table_delimiter_row) @punctuation.special"
            ),
        ),
        INLINE => (
            tree_sitter_md::INLINE_LANGUAGE.into(),
            include_str!("queries/markdown-inline.scm"),
        ),
        "toml" => (
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        ),
        "yaml" => (
            tree_sitter_yaml::LANGUAGE.into(),
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
        ),
        "json" => (
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
        ),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        ),
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            include_str!("queries/typescript.scm"),
        ),
        "python" => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY,
        ),
        "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        ),
        "ini" => (
            tree_sitter_ini::LANGUAGE.into(),
            tree_sitter_ini::HIGHLIGHTS_QUERY,
        ),
        "xml" => (
            tree_sitter_xml::LANGUAGE_XML.into(),
            tree_sitter_xml::XML_HIGHLIGHT_QUERY,
        ),
        "html" => (
            tree_sitter_html::LANGUAGE.into(),
            tree_sitter_html::HIGHLIGHTS_QUERY,
        ),
        "css" => (
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY,
        ),
        _ => unreachable!("registered grammar"),
    }
}

/// A virtual leading blank line disables the bundled grammar's metadata dialect.
/// The YAML pass already handled valid frontmatter. Remove the virtual byte
/// from tree positions before using any captures or injection ranges.
fn parse(
    parser: &mut Parser,
    source: &[u8],
    ranges: &[Range],
    markdown: bool,
    deadline: Instant,
) -> EngineResult<Tree> {
    let mut shifted = Vec::new();
    if markdown && !ranges.is_empty() {
        shifted.push(Range {
            start_byte: 0,
            end_byte: 1,
            start_point: Point::new(0, 0),
            end_point: Point::new(1, 0),
        });
        shifted.extend(ranges.iter().map(|range| Range {
            start_byte: range.start_byte + 1,
            end_byte: range.end_byte + 1,
            start_point: Point::new(range.start_point.row + 1, range.start_point.column),
            end_point: Point::new(range.end_point.row + 1, range.end_point.column),
        }));
    }
    parser
        .set_included_ranges(if shifted.is_empty() { ranges } else { &shifted })
        .map_err(|_| Fallback::Worker)?;
    let mut callback = |_: &tree_sitter::ParseState| progress(deadline);
    let options = ParseOptions::new().progress_callback(&mut callback);
    let mut tree = parser
        .parse_with_options(
            &mut |offset, _| {
                if markdown && offset == 0 {
                    return &b"\n"[..];
                }
                &source[(offset - usize::from(markdown)).min(source.len())..]
            },
            None,
            Some(options),
        )
        .ok_or_else(|| {
            if Instant::now() >= deadline {
                Fallback::Timeout
            } else {
                Fallback::Worker
            }
        })?;
    if markdown {
        tree.edit(&InputEdit {
            start_byte: 0,
            old_end_byte: 1,
            new_end_byte: 0,
            start_position: Point::new(0, 0),
            old_end_position: Point::new(1, 0),
            new_end_position: Point::new(0, 0),
        });
    }
    Ok(tree)
}

/// Limit a node's bounding range to the disjoint bytes actually parsed by its layer.
fn intersect(range: Range, bounds: &[Range]) -> impl Iterator<Item = Range> + '_ {
    let first = bounds.partition_point(|bound| bound.end_byte <= range.start_byte);
    bounds.is_empty().then_some(range).into_iter().chain(
        bounds[first..]
            .iter()
            .take_while(move |bound| bound.start_byte < range.end_byte)
            .map(move |bound| {
                let (start_byte, start_point) = if bound.start_byte > range.start_byte {
                    (bound.start_byte, bound.start_point)
                } else {
                    (range.start_byte, range.start_point)
                };
                let (end_byte, end_point) = if bound.end_byte < range.end_byte {
                    (bound.end_byte, bound.end_point)
                } else {
                    (range.end_byte, range.end_point)
                };
                Range {
                    start_byte,
                    end_byte,
                    start_point,
                    end_point,
                }
            }),
    )
}

fn slices<'a>(
    range: Range,
    bounds: &'a [Range],
    source: &'a [u8],
) -> impl Iterator<Item = &'a [u8]> {
    intersect(range, bounds).map(|range| &source[range.start_byte..range.end_byte])
}
