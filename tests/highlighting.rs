//! End-to-end helper behavior through the shipped executable and its private pipes.

use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

/// The wire category for level-one headings, independent of the active palette.
const HEADING_1: u8 = 15;

/// Build a worker request from original logical bytes and one scalar-aligned viewport.
fn request(language: u8, text: &str, rules: &[(u8, &str)]) -> Vec<u8> {
    let mut input = vec![b'O', language];
    push(&mut input, 0); // no fence aliases
    push(&mut input, rules.len() as u32);
    for (id, source) in rules {
        input.push(*id);
        push(&mut input, source.len() as u32);
        input.extend_from_slice(source.as_bytes());
    }
    push(&mut input, text.len() as u32);
    input.extend_from_slice(text.as_bytes());
    input.push(b'P');
    push(&mut input, 1);
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    for (offset, scalar) in text.char_indices() {
        let offset = offset as u32;
        let bytes = scalar.len_utf8() as u32;
        if let Some(last) = runs.last_mut()
            && last.1 == offset
            && last.2 == bytes
        {
            last.1 += bytes;
        } else {
            runs.push((offset, offset + bytes, bytes));
        }
    }
    push(&mut input, runs.len() as u32);
    for (start, end, stride) in runs {
        push(&mut input, start);
        push(&mut input, end);
        push(&mut input, stride);
    }
    input
}

fn push(packet: &mut Vec<u8>, number: u32) {
    packet.extend_from_slice(&number.to_le_bytes());
}

/// Exercise the production CLI before any terminal modes or configuration paths are opened.
fn run_worker(packet: Vec<u8>) -> Output {
    run_command(Command::new(env!("CARGO_BIN_EXE_ede")), packet)
}

fn run_command(mut command: Command, packet: Vec<u8>) -> Output {
    let mut worker = command
        .arg("--ede-syntax-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = worker.stdin.take().unwrap();
    let writer = std::thread::spawn(move || input.write_all(&packet));
    let output = worker.wait_with_output().unwrap();
    let _ = writer.join().unwrap();
    output
}

#[derive(Debug)]
struct Reply {
    arena: u32,
    native: u32,
    ranges: Vec<(u32, u32, u8)>,
}

impl Reply {
    fn parse(output: Output) -> Self {
        let data = output.stdout;
        assert!(
            output.status.success(),
            "{:?} {:?}: {}",
            output.status,
            data,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            data.starts_with(b"RRD"),
            "unexpected response {data:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let number = |index| u32::from_le_bytes(data[index..index + 4].try_into().unwrap());
        assert_eq!(number(3), 1);
        let count = number(15) as usize;
        assert_eq!(data.len(), 19 + count * 9);
        Self {
            arena: number(7),
            native: number(11),
            ranges: (0..count)
                .map(|i| {
                    let start = 19 + i * 9;
                    (number(start), number(start + 4), data[start + 8])
                })
                .collect(),
        }
    }

    fn at(&self, text: &str, token: &str) -> Option<u8> {
        let at = text.find(token).unwrap() as u32;
        self.ranges
            .iter()
            .find(|&&(start, end, _)| start <= at && at < end)
            .map(|&(_, _, category)| category)
    }
}

/// H01: semantics and real native allocation routing with the bundled runtime on each platform.
#[test]
fn installed_worker_highlights_all_bundled_languages() {
    for (id, source, token, category) in [
        (1, "# Heading\n\nOrdinary text", "Heading", HEADING_1),
        (2, "[table]\nnumber = 42\n", "42", 2),
        (3, "top: &here 'é'\nvalue: *here\n", "value", 5),
        (4, r#"{"key": 42}"#, "\"key", 5),
        (5, "const n = `hi ${42}`;", "const", 3),
        (6, "interface Example<T> { value: T }", "Example", 4),
        (7, "def hello():\n    return f'hi {42}'\n", "42", 2),
        (8, "fn main() { println!(r#\"é\"#); }", "r#", 1),
        (9, "# comment\n[section]\nk=v ; text\nlast=value", "last", 5),
        (10, "<root a=\"é\">&amp;<![CDATA[<]]></root>", "&amp;", 8),
        (11, "<p class='hi'><!-- comment --></p>", "class", 13),
        (12, ".foo { color: #ffaabb; width: 2px; }", "color", 5),
    ] {
        let reply = Reply::parse(run_worker(request(id, source, &[])));
        assert_eq!(
            reply.at(source, token),
            Some(category),
            "{source:?}: {reply:?}"
        );
        assert!(
            reply.arena > 0 && reply.arena <= 64 * 1024 * 1024,
            "{reply:?}"
        );
        assert!(
            reply.native > 0,
            "C allocations bypassed the arena for grammar {id}"
        );
    }
}

/// H02/H09: nested original-byte offsets, unknown HTML type, and custom capture order.
#[test]
fn nested_markdown_html_and_user_query_priorities() {
    let source = concat!(
        "---\r\nname: é\r\n---\r\n\r\n",
        "~~old~~ and `~~code~~`\n\n",
        ">   ~~~HTML extra\n",
        ">   <script type=\"MODULE\">const js = 42;</script>\n",
        ">   <style type=' Text/CSS '>p { color: red; }</style>\n",
        ">   <script type=\"application/json\">{ \"n\": 11 }</script>\n",
        ">   ~~~\n",
        "\n```rust\nfn local() {}\n",
    );
    let rules = [(
        5,
        "((number) @type (#eq? @type \"42\"))\n((number) @keyword (#eq? @keyword \"42\"))",
    )];
    let reply = Reply::parse(run_worker(request(1, source, &rules)));
    for (token, category) in [
        ("name", 5),
        ("~~old~~", 6),
        ("`~~code~~`", 14),
        ("const js", 3),
        ("42", 3),
        ("color", 5),
        ("{ \"n\"", 14),
        ("fn local", 3),
    ] {
        assert_eq!(
            reply.at(source, token),
            Some(category),
            "{token}: {reply:?}"
        );
    }
}

/// H04/H08: real aggregate allocation failure in the installed subprocess.
#[test]
#[ignore = "run in release mode; records the real helper's arena and OS RSS"]
fn oversized_projection_reaches_fixed_heap_ceiling() {
    // A cheap parse followed by a deliberately oversized glyph projection
    // forces the heap limit independently of parser speed or CPU scheduling.
    let source = " ".repeat(4 * 1024 * 1024) + "0";
    let command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/time");
        command.args(["-l", env!("CARGO_BIN_EXE_ede")]);
        command
    } else {
        let mut command = Command::new("/usr/bin/time");
        command.args(["-v", env!("CARGO_BIN_EXE_ede")]);
        command
    };
    let output = run_command(command, request(4, &source, &[]));
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        output.status.code(),
        Some(73),
        "{:?} {:?}",
        output.status,
        output.stdout
    );
    assert_eq!(output.stdout, b"RR");
}

/// Ordinary CommonMark metadata lookalikes remain source constructs.
#[test]
fn strict_markdown_frontmatter_and_multiline_prefixes() {
    for source in [
        "--- \n# Ordinary heading\n---\n",
        "  ---\n# Ordinary heading\n---\n",
        "---\n# Ordinary heading\n",
    ] {
        let reply = Reply::parse(run_worker(request(1, source, &[])));
        assert_eq!(
            reply.at(source, "Ordinary"),
            Some(HEADING_1),
            "{source:?}: {reply:?}"
        );
    }
    let source = "+++\n*inside*\n+++\n";
    assert_eq!(
        Reply::parse(run_worker(request(1, source, &[]))).at(source, "*inside*"),
        Some(6)
    );
    let source = concat!(
        "> **first\n> second**\n\n",
        "> ~~~html\n> <script>const str = `first\n> rest`; let y = 2;</script>\n",
        "> <style type=\"\n> text/css \">p { color: red; }</style>\n> ~~~\n",
    );
    let reply = Reply::parse(run_worker(request(1, source, &[])));
    for (token, color) in [("> second", 10), ("> rest", 10), ("rest`", 1), ("color", 5)] {
        assert_eq!(reply.at(source, token), Some(color), "{token:?}: {reply:?}");
    }
}

/// User rules can address Markdown inline syntax and custom matches stay in their layer.
#[test]
fn markdown_inline_query_and_string_interpolations() {
    let source = "prefix *remark* and `literal`\n";
    let reply = Reply::parse(run_worker(request(
        1,
        source,
        &[(13, "(emphasis) @keyword")],
    )));
    assert_eq!(reply.at(source, "*remark*"), Some(3));
    assert_eq!(reply.at(source, "`literal`"), Some(14));

    for (id, source, expression, delimiter) in [
        (5, "const x = `begin ${value + 42} end`;", "value", "${"),
        (6, "const x = `begin ${value + 42} end`;", "value", "${"),
        (7, "x = f'begin {value + 42} end'\n", "value", "{"),
    ] {
        let reply = Reply::parse(run_worker(request(id, source, &[])));
        assert_eq!(reply.at(source, expression), None, "{source:?}: {reply:?}");
        assert_eq!(reply.at(source, "42"), Some(2), "{source:?}: {reply:?}");
        assert_eq!(
            reply.at(source, delimiter),
            Some(10),
            "{source:?}: {reply:?}"
        );
        assert_eq!(reply.at(source, "end"), Some(1), "{source:?}: {reply:?}");
    }
}

/// A Markdown grammar behaves the same when its fenced offsets have quote-prefix gaps.
#[test]
fn quoted_markdown_fence_has_logical_frontmatter() {
    let source = "> ~~~markdown\n> ---\n> title: nested\n> ---\n> # Done\n> ~~~\n";
    let reply = Reply::parse(run_worker(request(1, source, &[])));
    assert_eq!(reply.at(source, "title"), Some(5));
    assert_eq!(reply.at(source, "> title"), Some(10));
    assert_eq!(reply.at(source, "Done"), Some(HEADING_1));
}

/// Recovery still highlights unambiguous properties, values and punctuation around invalid text.
#[test]
fn incomplete_json_and_css_keep_distinct_styles() {
    let json = r#"{"key":42,"broken": ]"#;
    let reply = Reply::parse(run_worker(request(4, json, &[])));
    for (token, category) in [("{", 10), ("\"key\"", 5), (":", 10), ("42", 2), (",", 10)] {
        assert_eq!(
            reply.at(json, token),
            Some(category),
            "{token:?}: {reply:?}"
        );
    }
    let css = "p { display: grid; color: red; }\n.broken {";
    let reply = Reply::parse(run_worker(request(12, css, &[])));
    for (token, category) in [("display", 5), ("grid", 8), ("red", 8)] {
        assert_eq!(reply.at(css, token), Some(category), "{token:?}: {reply:?}");
    }
}
