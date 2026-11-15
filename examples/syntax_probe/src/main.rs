//! Standalone checks for the Stage 07 Tree-sitter candidates and query assets.

mod fixtures;
mod measure;

use std::{ops::Range, process::ExitCode, time::Instant};
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};

/// Check coverage and cancellation with the same fixture runner in debug and release.
fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    if let Some(argument) = args.next() {
        if args.next().is_none() && measure::run(&argument) {
            return ExitCode::SUCCESS;
        }
        eprintln!("usage: ede-syntax-probe [ordinary | near-limit | dense]");
        return ExitCode::from(2);
    }
    let start = Instant::now();
    fixtures::languages();
    fixtures::markdown();
    fixtures::nested();
    fixtures::custom_query();
    fixtures::cancellation();
    println!(
        "Tree-sitter feasibility fixtures passed in {:?}",
        start.elapsed()
    );
    ExitCode::SUCCESS
}

/// Capture from an upstream query, in original document coordinates.
#[derive(Debug)]
struct Capture<'query> {
    name: &'query str,
    bytes: Range<usize>,
    pattern: usize,
}

/// Parse all or selected ranges of the original source in a fresh parser.
fn parse(language: &Language, source: &str, ranges: &[tree_sitter::Range]) -> Tree {
    let mut parser = Parser::new();
    parser.set_language(language).unwrap();
    parser.set_included_ranges(ranges).unwrap();
    parser.parse(source, None).unwrap()
}

/// Collect raw captures before any editor-specific priority or category mapping.
fn captures<'query>(query: &'query Query, tree: &Tree, source: &str) -> Vec<Capture<'query>> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut out = Vec::new();
    while let Some(matched) = matches.next() {
        out.extend(matched.captures().iter().map(|capture| Capture {
            name: query.capture_names()[capture.index as usize],
            bytes: capture.node.byte_range(),
            pattern: matched.pattern_index,
        }));
    }
    out
}

/// Require a capture at exactly the bytes of the given fixture token.
fn expect(source: &str, captures: &[Capture<'_>], name: &str, token: &str) {
    let start = source.find(token).expect("token missing in fixture source");
    let bytes = start..start + token.len();
    assert!(
        captures
            .iter()
            .any(|item| item.name == name && item.bytes == bytes),
        "Missing {name:?} for {token:?} at {bytes:?}: {captures:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::fixtures;

    #[test]
    fn bundled_language_queries() {
        fixtures::languages();
    }

    #[test]
    fn markdown_extensions() {
        fixtures::markdown();
    }

    #[test]
    fn original_nested_coordinates() {
        fixtures::nested();
    }

    #[test]
    fn user_query_validation_and_priority() {
        fixtures::custom_query();
    }

    #[test]
    fn cooperative_interruption() {
        fixtures::cancellation();
    }
}
