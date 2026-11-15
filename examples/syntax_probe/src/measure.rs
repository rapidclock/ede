//! Measure one deterministic parser workload per process for separate peak RSS samples.

use std::{
    ffi::OsStr,
    ops::ControlFlow,
    time::{Duration, Instant},
};
use tree_sitter::{
    ParseOptions, Parser, Query, QueryCursor, QueryCursorOptions, StreamingIterator,
};

const DEADLINE: Duration = Duration::from_millis(250);

/// Time root parse plus upstream queries with cooperative deadline checks.
/// The caller measures RSS separately using the platform's `time` utility.
pub(super) fn run(corpus: &OsStr) -> bool {
    let (language, rules, source) = match corpus.to_str() {
        Some("ordinary") => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            "fn demo() { let value: u64 = 42; println!(\"{value}\"); }\n".repeat(128),
        ),
        Some("near-limit") => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            // 512 lines totalling 20 MiB, each one below the 64 KiB line cap.
            format!("// {}\n", "x".repeat(40 * 1024 - 4)).repeat(512),
        ),
        Some("dense") => (
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
            format!("[{}0]", "[1,2],\n".repeat(600_000)),
        ),
        _ => return false,
    };
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    let query = Query::new(&language, rules).unwrap();
    let mut cursor = QueryCursor::new();

    let start = Instant::now();
    let check = || {
        if start.elapsed() < DEADLINE {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    };
    let mut progress = |_: &tree_sitter::ParseState| check();
    let parse_options = ParseOptions::new().progress_callback(&mut progress);
    let tree = parser.parse_with_options(
        &mut |i, _| &source.as_bytes()[i..],
        None,
        Some(parse_options),
    );
    let parsed = start.elapsed();
    let mut count = 0usize;
    if let Some(tree) = tree {
        let mut progress = |_: &tree_sitter::QueryCursorState| check();
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        let mut matches =
            cursor.matches_with_options(&query, tree.root_node(), source.as_bytes(), options);
        while check().is_continue()
            && let Some(matched) = matches.next()
        {
            count += matched.captures().len();
        }
    }
    let elapsed = start.elapsed();
    println!(
        "{corpus:?}: {} bytes, {:?} parse, {:?} total, {count} captures, {}",
        source.len(),
        parsed,
        elapsed,
        if elapsed < DEADLINE {
            "complete"
        } else {
            "deadline reached"
        }
    );
    true
}
