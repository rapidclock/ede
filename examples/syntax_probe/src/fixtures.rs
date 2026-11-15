//! Candidate checks on representative source and original byte coordinates.

use super::{captures, expect, parse};
use std::ops::ControlFlow;
use tree_sitter::{
    Language, ParseOptions, Parser, Query, QueryCursor, QueryCursorOptions, StreamingIterator,
};

/// Exercise published captures across the language registry and report each ABI.
pub(super) fn languages() {
    let cases = [
        (
            "TOML",
            tree_sitter_toml_ng::LANGUAGE,
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            "[build]\nlevel = 42 # setting\n",
            "number",
            "42",
        ),
        (
            "YAML",
            tree_sitter_yaml::LANGUAGE,
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            "name: &item 'é'\nother: *item\n",
            "property",
            "other",
        ),
        (
            "JSON",
            tree_sitter_json::LANGUAGE,
            tree_sitter_json::HIGHLIGHTS_QUERY,
            r#"{"name": "é", "count": 42}"#,
            "string.special.key",
            "\"count\"",
        ),
        (
            "JavaScript",
            tree_sitter_javascript::LANGUAGE,
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            "const greeting = `hi ${value + 42}`;",
            "number",
            "42",
        ),
        (
            "TypeScript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "interface Item<T> { value: T }",
            "type",
            "Item",
        ),
        (
            "Python",
            tree_sitter_python::LANGUAGE,
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "def hello():\n    return f'hi {42}'\n",
            "number",
            "42",
        ),
        (
            "Rust",
            tree_sitter_rust::LANGUAGE,
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            "fn main() { println!(r#\"é\"#); }",
            "string",
            "r#\"é\"#",
        ),
        (
            "INI",
            tree_sitter_ini::LANGUAGE,
            tree_sitter_ini::HIGHLIGHTS_QUERY,
            "# heading\n[section]\nitem=value ; # text\nempty=\n; trailing\n",
            "type",
            "section",
        ),
        (
            "XML",
            tree_sitter_xml::LANGUAGE_XML,
            tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            "<root a=\"é\">&amp;<![CDATA[<]]></root>",
            "constant",
            "&amp;",
        ),
        (
            "HTML",
            tree_sitter_html::LANGUAGE,
            tree_sitter_html::HIGHLIGHTS_QUERY,
            "<section class='é'><!-- note --></section>",
            "attribute",
            "class",
        ),
        (
            "CSS",
            tree_sitter_css::LANGUAGE,
            tree_sitter_css::HIGHLIGHTS_QUERY,
            ".foo { color: #ffaabb; width: 2px; }",
            "property",
            "color",
        ),
    ];
    for (name, function, rules, source, category, token) in cases {
        let language = function.into();
        let tree = parse(&language, source, &[]);
        assert!(
            !tree.root_node().has_error(),
            "{name}: {}",
            tree.root_node().to_sexp()
        );
        let query = Query::new(&language, rules).unwrap_or_else(|error| panic!("{name}: {error}"));
        let items = captures(&query, &tree, source);
        expect(source, &items, category, token);
        println!(
            "{name}: ABI {}, {} bundled capture patterns",
            language.abi_version(),
            query.pattern_count()
        );
        if name == "INI" {
            let value = Query::new(&language, "(setting_value) @value").unwrap();
            expect(
                source,
                &captures(&value, &tree, source),
                "value",
                "value ; # text",
            );
        }
    }
}

/// Verify the Markdown syntax trees expose the required extensions to custom queries.
pub(super) fn markdown() {
    let block: Language = tree_sitter_md::LANGUAGE.into();
    let inline: Language = tree_sitter_md::INLINE_LANGUAGE.into();
    let source = "# Title\n\n- [x] Done\n\n| name |\n| --- |\n| cell |\n";
    let tree = parse(&block, source, &[]);
    let rules = format!(
        "{}\n(task_list_marker_checked) @task\n(pipe_table_cell) @cell",
        tree_sitter_md::HIGHLIGHT_QUERY_BLOCK
    );
    let query = Query::new(&block, &rules).unwrap();
    let items = captures(&query, &tree, source);
    expect(source, &items, "text.title", "Title");
    assert!(
        items
            .iter()
            .any(|item| item.name == "task" && source[item.bytes.clone()].trim() == "[x]"),
        "{items:?}"
    );
    assert_eq!(
        items.iter().filter(|item| item.name == "cell").count(),
        2,
        "{items:?}"
    );

    let source = "*boldish* ~~old~~ `~~code~~` [link](target)";
    let tree = parse(&inline, source, &[]);
    let rules = format!(
        "{}\n(strikethrough) @strike",
        tree_sitter_md::HIGHLIGHT_QUERY_INLINE
    );
    let query = Query::new(&inline, &rules).unwrap();
    let items = captures(&query, &tree, source);
    expect(source, &items, "text.emphasis", "*boldish*");
    expect(source, &items, "strike", "~~old~~");
    expect(source, &items, "text.literal", "`~~code~~`");
    expect(source, &items, "text.uri", "target");
    let code = source.find('`').unwrap();
    assert!(
        items
            .iter()
            .filter(|item| item.name == "strike")
            .all(|item| item.bytes.end <= code),
        "{items:?}"
    );
    println!(
        "Markdown block/inline ABIs: {}/{}",
        block.abi_version(),
        inline.abi_version()
    );
}

/// Use included ranges for Markdown → YAML and Markdown → HTML → JS/CSS.
pub(super) fn nested() {
    let source = concat!(
        "---\r\ntitle: é\r\n---\r\n\r\n",
        "  ~~~HTML extra\r\n",
        "  <script type=\"module\">const answer = 42;</script>\r\n",
        "  <style>p { color: red; }</style>\r\n",
        "  ~~~\r\n",
    );
    let markdown = tree_sitter_md::LANGUAGE.into();
    let root = parse(&markdown, source, &[]);
    let tree = root.root_node();
    let mut cursor = tree.walk();
    assert!(
        tree.named_children(&mut cursor)
            .any(|node| node.kind() == "minus_metadata")
    );

    // Metadata delimiters represent whole lines in ede; pass just the body to YAML.
    let yaml = tree_sitter_yaml::LANGUAGE.into();
    let yaml_end = source.find("---\r\n\r\n").unwrap();
    let frontmatter = range(source, "---\r\n".len(), yaml_end);
    let yaml_tree = parse(&yaml, source, &[frontmatter]);
    let yaml_query = Query::new(&yaml, tree_sitter_yaml::HIGHLIGHTS_QUERY).unwrap();
    expect(
        source,
        &captures(&yaml_query, &yaml_tree, source),
        "property",
        "title",
    );

    let markdown_query =
        Query::new(&markdown, "(info_string) @info\n(code_fence_content) @body").unwrap();
    let items = captures(&markdown_query, &root, source);
    expect(source, &items, "info", "HTML extra");
    let body = items.iter().find(|item| item.name == "body").unwrap();
    let html = tree_sitter_html::LANGUAGE.into();
    let html_tree = parse(
        &html,
        source,
        &[range(source, body.bytes.start, body.bytes.end)],
    );
    let injections = Query::new(
        &html,
        "(script_element (raw_text) @js)\n(style_element (raw_text) @css)",
    )
    .unwrap();
    let items = captures(&injections, &html_tree, source);
    expect(source, &items, "js", "const answer = 42;");
    expect(source, &items, "css", "p { color: red; }");
    for (id, language, rules, category, token) in [
        (
            "js",
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            "keyword",
            "const",
        ),
        (
            "css",
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "property",
            "color",
        ),
    ] {
        let bytes = items
            .iter()
            .find(|item| item.name == id)
            .unwrap()
            .bytes
            .clone();
        assert!(body.bytes.start <= bytes.start && bytes.end <= body.bytes.end);
        let tree = parse(&language, source, &[range(source, bytes.start, bytes.end)]);
        let query = Query::new(&language, rules).unwrap();
        expect(source, &captures(&query, &tree, source), category, token);
    }
}

/// Convert original bytes to the byte-column point pair that included ranges require.
fn range(source: &str, start: usize, end: usize) -> tree_sitter::Range {
    let point = |offset| {
        let before = &source.as_bytes()[..offset];
        let row = before.iter().filter(|&&byte| byte == b'\n').count();
        let column = before
            .iter()
            .rev()
            .take_while(|&&byte| byte != b'\n')
            .count();
        tree_sitter::Point::new(row, column)
    };
    tree_sitter::Range {
        start_byte: start,
        end_byte: end,
        start_point: point(start),
        end_point: point(end),
    }
}

/// A user can match a parsed token with a predicate and override the bundled capture.
pub(super) fn custom_query() {
    let language = tree_sitter_json::LANGUAGE.into();
    let source = "[42, 8]";
    let rules = format!(
        r#"{}
        ((number) @keyword (#eq? @keyword "42"))
        ((number) @type (#match? @type "^42$"))
    "#,
        tree_sitter_json::HIGHLIGHTS_QUERY
    );
    let tree = parse(&language, source, &[]);
    let query = Query::new(&language, &rules).unwrap();
    let items = captures(&query, &tree, source);
    let effective = |bytes| {
        items
            .iter()
            .filter(|item| item.bytes == bytes)
            .max_by_key(|item| item.pattern)
            .unwrap()
            .name
    };
    assert_eq!(effective(1..3), "type");
    assert_eq!(effective(5..6), "number");
    assert!(Query::new(&language, "(ruby_only_node) @keyword").is_err());
    assert!(Query::new(&language, r#"((number) @keyword (#match? @keyword "["))"#).is_err());
}

/// Request cancellation from a parse callback, an included-range parse and a cursor with no matches.
pub(super) fn cancellation() {
    let body = "42,\n".repeat(32_768);
    let source = format!("outside\n[{body}0]\nsuffix");
    let language = tree_sitter_json::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    for ranges in [
        vec![],
        vec![range(
            &source,
            "outside\n".len(),
            source.len() - "\nsuffix".len(),
        )],
    ] {
        parser.set_included_ranges(&ranges).unwrap();
        let mut interrupts = 0;
        let mut interrupt = |_: &tree_sitter::ParseState| {
            interrupts += 1;
            ControlFlow::Break(())
        };
        let options = ParseOptions::new().progress_callback(&mut interrupt);
        assert!(
            parser
                .parse_with_options(&mut |i, _| &source.as_bytes()[i..], None, Some(options))
                .is_none()
        );
        assert_eq!(interrupts, 1);
        parser.reset();
    }
    let tree = parser.parse(&source, None).unwrap();
    // The source contains no strings in its included range. A callback must run
    // even when the cursor has no match to hand back to its caller.
    let query = Query::new(&language, "(string) @absent").unwrap();
    let mut cursor = QueryCursor::new();
    let mut interrupts = 0;
    let mut interrupt = |_: &tree_sitter::QueryCursorState| {
        interrupts += 1;
        ControlFlow::Break(())
    };
    let options = QueryCursorOptions::new().progress_callback(&mut interrupt);
    {
        let mut matches =
            cursor.matches_with_options(&query, tree.root_node(), source.as_bytes(), options);
        assert!(matches.next().is_none());
    }
    assert_eq!(interrupts, 1);
}
