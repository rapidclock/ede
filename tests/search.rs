//! Search acceptance checks use actual paged revisions rather than saved-file shortcuts.
use ede::{
    document::{Cancellation, Document, Edit, OpenOptions, TextRange},
    search::{Query, ScanOptions},
};

/// Capture canonical ranges under deliberately tiny scalar-aligned scan windows.
fn ranges(text: &str, query: &str, case: bool, word: bool) -> Vec<(u64, u64)> {
    let cancel = Cancellation::default();
    let mut doc = Document::new(OpenOptions::default()).unwrap();
    doc.apply(
        doc.revision(),
        [Edit {
            range: TextRange::new(0, 0).unwrap(),
            text,
        }],
        &cancel,
    )
    .unwrap();
    let snapshot = doc.snapshot().unwrap();
    let matcher = Query {
        literal: query.into(),
        match_case: case,
        whole_word: word,
    }
    .compile()
    .unwrap();
    let mut actual = Vec::new();
    matcher
        .scan(
            &snapshot,
            &cancel,
            ScanOptions {
                chunk_bytes: 4,
                ..ScanOptions::default()
            },
            |range| {
                actual.push((range.start().0, range.end().0));
                Ok(true)
            },
            |_| {},
        )
        .unwrap();
    actual
}

#[test]
fn canonical_literal_matches_cross_windows_pieces_and_line_endings() {
    assert_eq!(ranges("aaa aa", "aa", true, false), [(0, 2), (4, 6)]);
    assert_eq!(ranges("-$1.*-", "$1.*", true, false), [(1, 5)]);
    assert_eq!(ranges("aa\r\nbb\naa\nbb", "aa\nbb", true, false), [(7, 12)]);
    assert_eq!(ranges("a\r\nb\nc", "\n", true, false), [(4, 5)]);
    assert_eq!(ranges("a\r\nb\nc", "\r\n", true, false), [(1, 3)]);
    assert!(ranges("anything", "", true, false).is_empty());
}

#[test]
fn unicode_case_word_and_original_offsets() {
    assert_eq!(ranges("KkK", "k", false, false), [(0, 1), (1, 2), (2, 5)]);
    assert_eq!(ranges("Σσς", "σ", false, false), [(0, 2), (2, 4), (4, 6)]);
    assert_eq!(ranges("ß ss", "ss", false, false), [(3, 5)]);
    assert_eq!(ranges("e\u{301} é", "é", false, false), [(4, 6)]);
    assert_eq!(
        ranges("xfoo foo_ foo\u{301} foo\u{200d} foo", "foo", true, true),
        [(23, 26)]
    );
}

#[test]
fn cancellation_and_early_stop_are_observed_with_dense_matches() {
    let cancel = Cancellation::default();
    let mut doc = Document::new(OpenOptions::default()).unwrap();
    doc.apply(
        doc.revision(),
        [Edit {
            range: TextRange::new(0, 0).unwrap(),
            text: &"a".repeat(10000),
        }],
        &cancel,
    )
    .unwrap();
    let query = Query {
        literal: "a".into(),
        match_case: true,
        whole_word: false,
    }
    .compile()
    .unwrap();
    let mut count = 0;
    let result = query.scan(
        &doc.snapshot().unwrap(),
        &cancel,
        ScanOptions::default(),
        |_| {
            count += 1;
            if count == 257 {
                cancel.cancel();
            }
            Ok(true)
        },
        |_| {},
    );
    assert!(matches!(result, Err(ede::document::Error::Cancelled)));
    assert!(count < 514);
}

/// Independent scalar-by-scalar model, including invalid CRLF endpoints and word context.
fn reference(text: &str, query: &str, word: bool) -> Vec<(u64, u64)> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let end = at + query.len();
        let crlf = |offset: usize| {
            offset > 0
                && offset < text.len()
                && text.as_bytes()[offset - 1] == b'\r'
                && text.as_bytes()[offset] == b'\n'
        };
        if text[at..].starts_with(query)
            && !crlf(at)
            && !crlf(end)
            && (!word
                || (!text[..at]
                    .chars()
                    .next_back()
                    .is_some_and(regex_syntax::is_word_character)
                    && !text[end..]
                        .chars()
                        .next()
                        .is_some_and(regex_syntax::is_word_character)))
        {
            result.push((at as u64, end as u64));
            at = end;
        } else {
            at += text[at..].chars().next().unwrap().len_utf8();
        }
    }
    result
}

#[test]
fn randomized_canonical_ranges_match_scalar_reference() {
    let alphabet = ['a', 'b', ' ', '\r', '\n', '界', '\u{301}', '_'];
    let mut seed = 0x8d23_7751_u64;
    for _ in 0..80 {
        let mut text = String::new();
        for _ in 0..96 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push(alphabet[(seed >> 32) as usize % alphabet.len()]);
        }
        let chars: Vec<_> = text.chars().collect();
        let index = seed as usize % (chars.len() - 4);
        let query: String = chars[index..index + (seed as usize % 4 + 1)]
            .iter()
            .collect();
        for word in [false, true] {
            assert_eq!(
                ranges(&text, &query, true, word),
                reference(&text, &query, word),
                "text={text:?}, query={query:?}, word={word}"
            );
        }
    }
}

#[test]
fn matches_span_piece_boundaries_and_ignore_serialization_bom() {
    use ede::document::ByteOffset;
    let cancel = Cancellation::default();
    let mut doc = Document::new(OpenOptions::default()).unwrap();
    doc.apply(
        doc.revision(),
        [Edit {
            range: TextRange::new(0, 0).unwrap(),
            text: "pre-nee-post",
        }],
        &cancel,
    )
    .unwrap();
    doc.apply(
        doc.revision(),
        [Edit {
            range: TextRange::new(7, 7).unwrap(),
            text: "dle",
        }],
        &cancel,
    )
    .unwrap();
    let query = Query {
        literal: "needle".into(),
        ..Query::default()
    }
    .compile()
    .unwrap();
    let mut matches = Vec::new();
    query
        .scan(
            &doc.snapshot().unwrap(),
            &cancel,
            ScanOptions::default(),
            |range| {
                matches.push((range.start().0, range.end().0));
                Ok(true)
            },
            |_| {},
        )
        .unwrap();
    assert_eq!(matches, [(4, 10)]);

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bom");
    std::fs::write(&path, b"\xef\xbb\xbfneedle\r\nneedle").unwrap();
    let doc = Document::open(&path, OpenOptions::default(), &cancel, |_| {}).unwrap();
    let snapshot = doc.snapshot().unwrap();
    assert!(snapshot.format().bom);
    assert_eq!(
        snapshot.read_window(ByteOffset(0), 64).unwrap().text(),
        "needle\r\nneedle"
    );
    matches.clear();
    query
        .scan(
            &snapshot,
            &cancel,
            ScanOptions::default(),
            |range| {
                matches.push((range.start().0, range.end().0));
                Ok(true)
            },
            |_| {},
        )
        .unwrap();
    assert_eq!(matches, [(0, 6), (8, 14)]);
}

#[test]
fn maximum_folded_query_keeps_original_offsets_across_overlap() {
    let text = format!("{}{} tail", "-".repeat(65533), "K".repeat(4096));
    assert_eq!(
        ranges(&text, &"k".repeat(4096), false, false),
        [(65533, 65533 + 12288)]
    );
}
