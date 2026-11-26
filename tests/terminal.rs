//! Unix pseudo-terminal acceptance tests against the actual executable and kernel termios.
//!
//! These exercise terminal protocols and job control, not a graphical terminal emulator.

use std::{
    fs::File,
    io::{self, Read, Write},
    num::NonZeroU16,
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    libc,
    pty::{Winsize, openpty},
    sys::{
        signal::{Signal, kill},
        termios::{self, InputFlags, SetArg, Termios},
    },
    unistd::{Pid, setsid},
};

/// Bounded timeouts turn a broken terminal loop into a useful test failure, not a hanging CI job.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Minimize and restore either search mode through real keyboard and mouse input.
#[test]
fn search_minimize_restores_results_and_keeps_editor_focus() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fruit.txt");
    std::fs::write(
        &file,
        "green apple\napple tree\napple baked\n4\n5\n6\n7\n8\nlast line",
    )
    .unwrap();
    for shortcut in [b"\x06", b"\x08"] {
        let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&file));
        process.expect_status("Ln 1 Col 1");
        process.send(shortcut);
        process.expect("[−]  [×]");
        process.send(b"apple\r");
        process.expect("1 of 3 matches");
        process.send(b"\x1b[B");
        process.expect_status("Ln 2 Col 1");

        process.click_label("[−]", 1);
        process.expect(" · 3 matches");
        process.expect("[Restore]  [×]");
        process.expect_absent("Find:");
        process.send(b"\x1b[B");
        process.expect_status("Ln 3 Col 1");
        process.send(b"\x1b[27u");
        process.expect("[Restore]  [×]");
        process.send(b"\x06");
        process.expect("> 2:1 apple tree");
        process.expect_status("Ln 3 Col 1");
        assert_eq!(
            process.parser.screen().contents().contains("Replace:"),
            shortcut == b"\x08"
        );
        process.send(b"\t");
        process.expect(">Find: apple");
        process.send(b"\x1b[A\r");
        process.expect("[Restore]  [×]");
        process.resize(40, 8);
        process.expect("[Restore]  [×]");
        process.send(b"\x1b[1;5F");
        process.expect_status("Ln 9 Col 10");
        process.expect_body("last line", TIMEOUT);
        assert!(process.parser.screen().cursor_position().0 < 6);
        process.click_label("[Restore]", 1);
        process.expect(">Find: apple");
        process.expect_absent("[Restore]");
        process.send(b"\x1b[A\r");
        process.expect("[Restore]  [×]");
        process.click_label("[×]", 1);
        process.expect_absent("[Restore]");
        process.send(shortcut);
        process.expect(">Find: apple");
        process.expect_absent("3 matches");
        process.send(b"\x1b");
        process.expect_absent("Find:");
        process.send(b"\x1bOQ");
        process.expect("Actions (wheel/arrows)");
        process.click_label("Search", 1);
        process.expect("Find & Replace");
        for removed in [
            "Find next",
            "Find previous",
            "Replace next",
            "Replace all",
            "Cancel search",
        ] {
            assert!(!process.parser.screen().contents().contains(removed));
        }
        process.send(b"\x1b");
        process.expect_absent("Actions (wheel/arrows)");
        process.send(b"\x11");
        assert!(process.wait().success());
    }
}

/// A minimized Current tab search never offers rows from an edited or inactive tab.
#[test]
fn search_minimized_marks_changed_revision_and_current_tab_stale() {
    let root = tempfile::tempdir().unwrap();
    let left = root.path().join("left.txt");
    let right = root.path().join("right.txt");
    std::fs::write(&left, "apple left\n").unwrap();
    std::fs::write(&right, "apple right\n").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [left.as_os_str(), right.as_os_str()],
        None,
        Some(root.path()),
        None,
    );
    process.expect_body("apple left", TIMEOUT);
    process.expect_status("Ready | Ln 1 Col 1");
    process.send(b"\x06apple\r");
    process.expect("1 of 1 match");
    process.click_label("[−]", 1);
    process.expect("Find · 1 match");
    process.send(b"x");
    process.expect_body("xapple left", TIMEOUT);
    process.expect("Find · Stale results");
    process.send(b"\x1a\x06");
    process.expect(">Find: apple");
    process.expect_absent("1:1 apple left");
    process.send(b"\r");
    process.expect("1 of 1 match");
    process.click_label("[−]", 1);
    process.expect("Find · 1 match");
    process.send(b"\x1b[19~");
    process.expect_body("apple right", TIMEOUT);
    process.expect("Find · Stale results");
    process.send(b"\x06");
    process.expect(">Find: apple");
    process.expect_absent("1:1 apple left");
    process.send(b"\x1b");
    process.expect_absent("Find:");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// A fresh search starts at the first match and gives its arrow keys to the list.
#[test]
fn search_submission_selects_first_and_tabs_between_sections() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fruit.txt");
    std::fs::write(&file, "green apple\napple tree\napple baked").unwrap();
    for shortcut in [b"\x06", b"\x08"] {
        let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&file));
        process.expect_status("Ln 1 Col 1");
        process.send(b"\x1b[B\x1b[B");
        process.expect_status("Ln 3 Col 1");
        process.send(shortcut);
        process.expect(">Find:");
        process.send(b"apple\r");
        process.expect("1 of 3 matches");
        process.expect_status("Ln 1 Col 7");
        process.send(b"\x1b[B");
        process.expect_status("Ln 2 Col 1");
        process.send(b"\t");
        process.expect(">Find: apple");
        process.send(b"\t\x1b[B");
        process.expect_status("Ln 3 Col 1");
        process.send(b"\x1b[B\x1b[A");
        process.expect_status("Ln 2 Col 1");
        process.send(b"\x1b[Z");
        process.expect(">Find: apple");
        process.send(b"z\r");
        process.expect("No matches");
        process.expect(">Find: applez");
        process.send(b"\x1b");
        process.expect_absent("Options");
        process.send(b"\x11");
        assert!(process.wait().success());
    }
}

/// Search Details has real lines for individual outcomes and a divider above the form.
#[test]
fn search_details_use_separate_lines_and_a_soft_divider() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fruit.txt");
    std::fs::write(&file, "green apple\napple tree\n").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&file));
    process.expect_status("Ln 1 Col 1");
    process.send(b"\x06apple");
    process.expect(">Find: apple");
    process.click_label("[Find]", 1);
    process.expect("2 matches");
    process.expect("2:1 apple tree");
    let screen: Vec<_> = process.parser.screen().rows(0, 80).collect();
    let result = screen
        .iter()
        .position(|row| row.contains("2:1 apple tree"))
        .unwrap();
    assert!(
        screen[result + 1].contains("│─────────"),
        "expected a divider below the last result"
    );
    process.click_label("[Summary]", 1);
    process.expect("Search Details");
    process.expect("fruit.txt · 2 matches · Complete");
    assert!(!process.parser.screen().contents().contains("<U+000A>"));
    process.send(b"\x1b");
    process.expect_absent("Search Details");
    process.send(b"\x1b");
    process.expect_absent("Options");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Browse live search results with arrows and the mouse in the real terminal.
#[test]
fn search_window_browses_occurrences_without_tab() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fruit.txt");
    std::fs::write(&file, "green apple\napple tree\nbaked apple\n").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&file));
    process.expect_status("Ln 1 Col 1");
    process.send(b"\x06");
    process.expect("Options");
    process.click_label("Options", 1);
    process.expect("[ ] Match Whole Word");
    process.click_label("Options", 1);
    process.expect(">› Options");
    process.send(b"\x1b[Aapple\r");
    process.expect("1 of 3 matches");
    process.click_label("2:1", 1);
    process.expect_status("Ln 2 Col 1");
    process.send(b"\x1b[B");
    process.expect_status("Ln 3 Col 7");
    process.expect("3 of 3 matches");
    process.send(b"\x1b");
    process.expect_absent("Options");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Down and Up visit each expanded option in both modes without Tab.
#[test]
fn search_options_use_vertical_arrows() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fruit.txt");
    std::fs::write(&file, "apple\n").unwrap();
    for (open, to_options) in [(b'\x06', 1), (b'\x08', 3)] {
        let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&file));
        process.expect_status("Ln 1 Col 1");
        process.send(&[open]);
        process.expect("Options");
        process.send(&b"\x1b[B".repeat(to_options));
        process.expect(">› Options");
        process.send(b"\x1b[C\x1b[B\r\x1b[B\r");
        process.expect("[ ] Match case");
        process.expect("[x] Match Whole Word");
        for label in [">Tab<", ">LF<", ">CRLF<", ">Current tab<"] {
            process.send(b"\x1b[B");
            process.expect(label);
        }
        process.send(b"\r");
        process.expect(">All open tabs<");
        process.send(&b"\x1b[A".repeat(6));
        process.expect(">⌄ Options");
        process.send(b"\x1b[D");
        process.expect(">› Options");
        process.send(b"\x1b");
        process.expect_absent("Options");
        process.send(b"\x11");
        assert!(process.wait().success());
    }
}

/// The workspace preference is bounded, saved, and shared by both bottom-right windows.
#[test]
fn find_match_rows_settings_stepper_controls_both_windows() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fruit.txt");
    std::fs::write(&file, "apple\n".repeat(15)).unwrap();
    let open = || {
        PtyChild::editor_with_options(
            Some("xterm-256color"),
            [file.as_os_str()],
            None,
            Some(root.path()),
            None,
        )
    };
    let config = root.path().join("ede/config.toml");
    let check_list = |process: &mut PtyChild, rows: usize, close: bool| {
        process.expect(&format!("{rows}:1 apple"));
        let screen: Vec<_> = process.parser.screen().rows(0, 80).collect();
        let form = screen.iter().position(|row| row.contains("Find:")).unwrap();
        let last = screen
            .iter()
            .position(|row| row.contains(&format!("{rows}:1 apple")))
            .unwrap();
        assert!(
            last + 1 < form,
            "result list should be spaced above the form"
        );
        assert!(screen[last].find(&format!("{rows}:1 apple")).unwrap() >= 20);
        assert!(screen[22].ends_with('┘'));
        assert!(
            !screen
                .iter()
                .any(|row| row.contains(&format!("{}:1 apple", rows + 1)))
        );
        if close {
            process.send(b"\x1b");
            process.expect_absent("Options");
        }
    };
    let mut process = open();
    process.expect_status("Ln 1 Col 1");
    process.send(b"\x06apple\r");
    check_list(&mut process, 3, true);
    process.send(b"\x1b[15~");
    process.click_label("[Editor]", 1);
    process.expect("[−]  3  [+]");
    process.click_label("Find match rows", 1);
    process.send(&b"\x1b[D".repeat(4));
    process.expect("[−]  1  [+]");
    process.send(&b"\x1b[C".repeat(12));
    process.expect("[−] 10  [+]");
    process.click_label("[−]", 1);
    process.expect("[−]  9  [+]");
    process.click_label("[+]", 1);
    process.expect("[−] 10  [+]");
    assert!(!config.exists());
    process.click_label("[Save]", 1);
    process.expect_status("Settings saved");
    assert!(
        std::fs::read_to_string(config)
            .unwrap()
            .contains("find_match_rows = 10")
    );
    process.send(b"\x1b");
    process.expect_absent("Find match rows");
    process.send(b"\x08\r");
    check_list(&mut process, 10, true);
    process.send(b"\x11");
    assert!(process.wait().success());

    let mut restarted = open();
    restarted.expect_status("Ln 1 Col 1");
    restarted.send(b"\x06apple\r");
    check_list(&mut restarted, 10, true);
    restarted.send(b"\x1b[15~");
    restarted.click_label("[Editor]", 1);
    restarted.click_label("Find match rows", 1);
    restarted.send(&b"\x1b[D".repeat(9));
    restarted.expect("[−]  1  [+]");
    restarted.click_label("[Save]", 1);
    restarted.expect_status("Settings saved");
    restarted.send(b"\x1b");
    restarted.expect_absent("Find match rows");
    restarted.send(b"\x06\r");
    check_list(&mut restarted, 1, false);
    let row = restarted
        .parser
        .screen()
        .rows(0, 80)
        .position(|row| row.contains("1:1 apple"))
        .unwrap()
        + 1;
    restarted.send(format!("\x1b[<65;35;{row}M").as_bytes());
    restarted.expect("2:1 apple");
    restarted.expect_status("Ln 1 Col 1");
    restarted.click_label("2:1 apple", 1);
    restarted.expect_status("Ln 2 Col 1");
    restarted.send(b"\x1b");
    restarted.expect_absent("Options");
    restarted.send(b"\x11");
    assert!(restarted.wait().success());
}

/// The end of a dense list is reachable, and scrolling the list never moves the caret.
#[test]
fn search_window_pages_and_replaces_a_deep_match() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fruit.txt");
    std::fs::write(&file, "apple\n".repeat(60)).unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&file));
    process.expect_status("Ln 1 Col 1");
    process.send(b"\x08apple\r");
    process.expect("1 of 60 matches");
    process.click_label("2:1", 1);
    process.expect_status("Ln 2 Col 1");
    process.send(b"\x1b[F");
    process.expect_status("Ln 60 Col 1");
    process.expect("60 of 60 matches");
    let row = process
        .parser
        .screen()
        .rows(0, 80)
        .position(|row| row.contains("60:1"))
        .unwrap()
        + 1;
    process.send(format!("\x1b[<64;35;{row}M").as_bytes());
    process.expect("56:1");
    process.expect_status("Ln 60 Col 1");
    process.click_label("57:1", 1);
    process.expect_status("Ln 57 Col 1");
    process.click_label("[Replace Next]", 1);
    process.expect("Removed · 57:1");
    process.expect_status("Ln 58 Col 1");
    process.expect("57 of 59 matches");
    process.send(b"\x1b");
    process.expect_absent("Options");
    process.send(b"\x1a");
    process.expect_body("apple", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// All-open occurrence navigation can activate a different tab's unsaved text.
#[test]
fn search_window_jumps_to_another_unsaved_tab() {
    let root = tempfile::tempdir().unwrap();
    let left = root.path().join("left.txt");
    let right = root.path().join("right.txt");
    std::fs::write(&left, "apple left\n").unwrap();
    std::fs::write(&right, "prefix\nright\n").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [left.as_os_str(), right.as_os_str()],
        None,
        Some(root.path()),
        None,
    );
    process.expect_body("apple left", TIMEOUT);
    process.send(b"\x1b[19~");
    process.expect_body("prefix", TIMEOUT);
    process.expect_status("Ready | Ln 1 Col 1");
    process.send(b"\x1b[B");
    process.expect_status("Ln 2 Col 1");
    process.send(b"\x1b[200~apple \x1b[201~");
    process.expect_body("apple right", TIMEOUT);
    process.send(b"\x06");
    process.click_label("Options", 1);
    process.click_label("[Current tab]", 1);
    process.click_label("Find:", 1);
    process.send(b"apple\r");
    process.expect("1 of 2 matches");
    process.expect_body("apple left", TIMEOUT);
    process.click_label("2:1 right.txt", 1);
    process.expect_body("apple right", TIMEOUT);
    process.expect_status("Ln 2 Col 1");
    process.expect("2 of 2 matches");
    process.send(b"\x1b");
    process.expect_absent("Options");
    process.send(b"\x1a");
    process.expect_absent("apple");
    process.expect_body("right", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// A compact search shows its result count and keeps list focus across terminal resize.
#[test]
fn search_window_compact_results_survive_resize() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fruit.txt");
    std::fs::write(&file, "apple\napple\napple\n").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&file));
    process.expect_status("Ln 1 Col 1");
    process.resize(40, 8);
    process.send(b"\x06apple\r");
    process.expect("1 of 3 matches");
    process.expect("[Form]");
    process.send(b"\x1b[B");
    process.expect_status("Ln 2 Col 1");
    process.expect("2 of 3 matches");
    process.resize(80, 24);
    process.expect("Match Whole Word off");
    process.send(b"\t");
    process.expect(">Find:");
    process.click_label("3:1 apple", 1);
    process.expect_status("Ln 3 Col 1");
    process.resize(40, 8);
    process.expect("[Form]");
    process.expect("3:1 apple");
    process.send(b"\x1b[A");
    process.expect_status("Ln 2 Col 1");
    process.resize(80, 24);
    process.expect("Match Whole Word off");
    process.send(b"\x1b[B");
    process.expect_status("Ln 3 Col 1");
    process.send(b"\x1b[A");
    process.expect_status("Ln 2 Col 1");
    let last = process
        .parser
        .screen()
        .rows(0, 80)
        .position(|row| row.contains("3:1 apple"))
        .unwrap()
        + 1;
    process.send(format!("\x1b[<65;35;{last}M").as_bytes());
    process.resize(40, 8);
    process.expect("[Form]");
    process.expect("2:1 apple");
    process.send(b"\x1b");
    process.expect_absent("[Form]");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// A right-side occurrence stays visible above the fixed lower-right overlay.
#[test]
fn search_jump_reveals_match_above_anchored_window() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fruit.txt");
    let body = format!("{}{}orchard poirée\n", "plain\n".repeat(40), " ".repeat(60));
    std::fs::write(&file, body).unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&file));
    process.expect_status("Ln 1 Col 1");
    process.send("\x06poirée\r".as_bytes());
    process.expect("1 of 1 match");
    process.expect_status("Ln 41 Col 69");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        process.drain();
        let screen: Vec<_> = process.parser.screen().rows(0, 80).collect();
        if let Some(window) = screen.iter().position(|row| row.contains("┌ Find "))
            && screen[..window].iter().any(|row| {
                row.get(60..)
                    .is_some_and(|end| end.contains("orchard poirée"))
            })
        {
            assert!(screen[22].ends_with('┘'));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "match covered by the search window: {screen:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    process.send(b"\x1b");
    process.expect_absent("Options");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// A dirty outside dismissal preserves the pending field and requires an explicit choice.
#[test]
fn settings_outside_click_protects_unaccepted_text() {
    let root = tempfile::tempdir().unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        None,
        Some(root.path()),
        None,
    );
    process.resize(40, 8);
    process.expect("New Scratch");
    process.send(b"\x1b[15~");
    process.expect("Checkpoint interval");
    process.send(b"\r\x7f7\x1b[<0;40;8M\x1b[<0;40;8m");
    process.expect(">Keep Editing<");
    process.send(b"\x1b[<0;40;8M\x1b[<0;40;8m");
    process.expect(">Keep Editing<");
    assert!(!root.path().join("ede/config.toml").exists());
    process.send(b"\x1b");
    process.expect("Checkpoint interval (s) 7");
    process.send(b"\x1b[<0;40;8M\x1b[<0;40;8m");
    process.click_label("[Save and Exit]", 1);
    process.expect_status("Settings saved");
    let config = std::fs::read_to_string(root.path().join("ede/config.toml")).unwrap();
    assert!(config.contains("checkpoint_interval_seconds = 7"));
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Details reads an inactive tab without installing it in the editor.
#[test]
fn tabs_details_preserves_active_document() {
    let root = tempfile::tempdir().unwrap();
    let rust = root.path().join("inactive.rs");
    let text = root.path().join("active.txt");
    std::fs::write(&rust, "fn example() {}\n").unwrap();
    std::fs::write(&text, "active document text\n").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [rust.as_os_str(), text.as_os_str()],
        None,
        Some(root.path()),
        None,
    );
    process.expect_body("fn example()", TIMEOUT);
    process.send(b"\x1b[19~");
    process.expect_body("active document text", TIMEOUT);
    run_action(&mut process, "Tabs", 0);
    process.expect("Tabs — Enter to switch");
    process.send(b"\x1b[A");
    process.expect("> active.txt");
    syntax_color(&mut process, 3, 7, vt100::Color::Idx(6));
    syntax_color(&mut process, 4, 7, vt100::Color::Default);
    process.click_label("[Details]", 1);
    process.expect("Language: Rust (Auto)");
    process.expect("Highlighting: analysis runs when the tab is active");
    process.send(b"\x1b");
    process.expect(">Details<");
    process.send(b"\r");
    process.expect("Language: Rust (Auto)");
    process.send(b"\x1b");
    process.expect(">Details<");
    process.send(b"\x1b[<0;80;1M\x1b[<0;80;1m");
    process.expect_absent("Tabs — Enter to switch");
    process.expect_body("active document text", TIMEOUT);

    let config = root.path().join("ede");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("highlighting.toml"),
        "[files.extensions]\nrs = 'plain'\n",
    )
    .unwrap();
    run_action(&mut process, "Highlighting", 2);
    process.expect_absent("Actions (wheel/arrows)");
    run_action(&mut process, "Tabs", 0);
    process.expect("Tabs — Enter to switch");
    process.send(b"\x1b[A");
    process.click_label("[Details]", 1);
    process.expect("Language: Plain Text (Auto)");
    process.expect("Highlighting: Plain Text");
    process.send(b"\x1b");
    process.expect(">Details<");
    process.send(b"\x1b");
    process.expect_absent("Tabs — Enter to switch");
    process.expect_body("active document text", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Parked stream metadata stays live while an ordinary document owns the viewport.
#[test]
fn tab_details_tracks_parked_stream_without_switching() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("follow.log");
    std::fs::write(&log, "first\n").unwrap();
    let mut process = PtyChild::editor_with_args(
        Some("xterm-256color"),
        [std::ffi::OsStr::new("--follow"), log.as_os_str()],
    );
    process.expect_status("Waiting for more");
    process.send(b"\x0e");
    process.expect_status("Ready");
    process.send(b"active scratch");
    process.expect_body("active scratch", TIMEOUT);
    process.click_label("[Tabs]", 1);
    process.send(b"\x1b[A");
    process.click_label("[Details]", 1);
    process.expect("Read-only retained history — 6 bytes");
    process.expect_absent("Retry refresh");
    process.expect_absent("Line ending:");
    let mut file = File::options().append(true).open(&log).unwrap();
    file.write_all(b"second\n").unwrap();
    process.expect("Read-only retained history — 13 bytes");
    process.send(b"\x1b");
    process.expect(">Details<");
    process.send(b"\x1b");
    process.expect_absent("Tabs — Enter to switch");
    process.expect_body("active scratch", TIMEOUT);
    process.send(b"\x11");
    process.expect("discard retained stream history");
    process.send(b"\x1b[Z\r");
    assert!(process.wait().success());
}

/// Settings sections share a draft, and About exposes build and configuration information.
#[test]
fn settings_sections_and_menu_about_at_minimum_size() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        None,
        Some(root.path()),
        None,
    );
    process.resize(40, 8);
    process.expect("New Scratch");
    process.send(b"\x1b[15~");
    process.expect("Checkpoint interval");
    process.expect_absent("Document limit");
    process.expect_absent("[Config]");
    process.send(b"\r\x7fx");
    process.click_label("[Limits]", 1);
    process.expect("Enter a whole number from 1 to 3600");
    process.send(b"\x7f9");
    process.click_label("[Limits]", 1);
    process.expect("Document limit");
    process.expect_absent("Checkpoint interval");
    process.click_label("[Keys]", 1);
    process.expect("Command");
    process.expect("Shortcut");
    process.expect("App commands");
    process.send(b"\t\x1b[C\x1b[C\x1b[B");
    process.expect("Focused buttons");
    process.expect_absent("controls.activate");
    process.click_label("Activate button", 1);
    process.expect("controls.activate");
    process.click_label("[Replace]", 1);
    process.expect("Type the replacement shortcut");
    process.send(b"\x7f\x7f\x7f\x7f\x7fF10\r");
    process.expect("1/1 F10 (Custom)");
    process.click_label("[Save]", 1);
    process.expect_status("Settings saved");
    let config = std::fs::read_to_string(root.path().join("ede/config.toml")).unwrap();
    assert!(config.contains("checkpoint_interval_seconds = 9"));
    assert!(config.contains("[keybindings.controls]") && config.contains("activate = [\"F10\"]"));
    process.send(b"\x1b\x1bOQ\x1b[F\x1b[C\x1b[B\r");
    process.expect(concat!("ede ", env!("CARGO_PKG_VERSION")));
    process.resize(100, 18);
    process.expect("ede/config.toml");
    process.send(b"\x1b");
    process.expect_absent("About");
    process.send(b"\x1bOQ");
    process.click_label(" Help", 1);
    process.click_label(" About", 1);
    process.expect(concat!("ede ", env!("CARGO_PKG_VERSION")));
    process.send(b"\x1b");
    process.expect_absent("About");
    process.send(b"\x1b[15~");
    process.click_label("[Keyboard Shortcuts]", 1);
    process.expect("Command");
    process.expect("Shortcut");
    process.expect("Files");
    process.expect("Tabs");
    process.expect("While in Settings");
    process.click_label("Focused buttons", 1);
    process.expect("Button actions");
    process.expect("Button navigation");
    process.click_label("Activate button", 1);
    process.expect("1/1 F10 (Custom)");
    process.send(b"\x1b");
    process.expect_absent("controls.activate");
    process.send(b"\x1b");
    process.expect_absent("Shortcut");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Full configuration diagnostics are readable when the footer truncates them.
#[test]
fn settings_diagnostic_opens_and_returns_to_settings() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("ede");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[recovery]\ncheckpoint_interval_seconds = 0\n",
    )
    .unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        None,
        Some(root.path()),
        None,
    );
    process.expect_status("Settings error");
    process.resize(40, 8);
    process.send(b"\x1b[15~");
    process.click_label("[Details]", 1);
    process.expect("Settings Details");
    process.send(b"\x1b[B\x1b[B\x1b[B");
    process.expect("3600");
    process.send(b"\x1b");
    process.expect("Checkpoint interval");
    process.click_label("[Keys]", 1);
    process.click_label("[›]", 2);
    process.expect("Focused buttons");
    process.click_label("Activate button", 1);
    process.expect("controls.activate");
    process.expect("[Details]");
    process.send(b"\x1b");
    process.expect_absent("controls.activate");
    process.send(b"\x1b");
    process.expect_absent("Shortcut");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Search dismissal consumes the first click, even when it lands in editor content.
#[test]
fn search_outside_click_preserves_caret_until_second_click() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("search.txt");
    std::fs::write(&path, "first\nsecond\n").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_status("Ln 1 Col 1");
    process.send(b"\x06");
    process.expect("Find:");
    process.send(b"\x1b[<0;3;3M\x1b[<0;3;3m");
    process.expect_absent("Find:");
    process.expect_status("Ln 1 Col 1");
    process.send(b"\x1b[<0;3;3M\x1b[<0;3;3m");
    process.expect_status("Ln 2 Col 3");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Outside clicks consume the gesture and the direct Settings control works at 40×8.
#[test]
fn actions_outside_click_and_direct_settings_at_minimum_size() {
    let root = tempfile::tempdir().unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        None,
        Some(root.path()),
        None,
    );
    process.resize(40, 8);
    process.expect("[Settings]");
    process.send(b"\x1bOQ");
    process.expect("Actions (wheel/arrows)");
    process.send(b"\x1b[<0;3;2M\x1b[<0;3;2m"); // Top-left popup border.
    process.send(b"\x1b[<0;39;1M\x1b[<32;10;3M\x1b[<0;39;1m");
    process.send(b"\x1b[<0;10;3M\x1b[<32;39;1M\x1b[<0;10;3m");
    process.expect("Actions (wheel/arrows)");
    process.send(b"\x1b[<0;39;1M\x1b[<0;39;1m");
    process.expect_absent("Actions (wheel/arrows)");
    process.send(b"\x1bOQ");
    process.expect("Actions (wheel/arrows)");
    process.send(b"\x1b[<0;17;1M\x1b[<0;17;1m");
    process.expect_absent("Actions (wheel/arrows)");
    process.expect_absent("Checkpoint interval");
    process.click_label("[Settings]", 1);
    process.expect("Checkpoint interval");
    process.send(b"\x1b");
    process.expect_absent("Checkpoint interval");
    process.send(b"\x1b[15~");
    process.expect("Checkpoint interval");
    process.send(b"\x1b");
    process.expect_absent("Checkpoint interval");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Save commits a still-focused scalar, and a failed scalar edit survives a resize.
#[test]
fn settings_footer_saves_pending_value() {
    let root = tempfile::tempdir().unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        None,
        Some(root.path()),
        None,
    );
    process.resize(40, 8);
    process.expect("New Scratch");
    process.send(b"\x1b[15~");
    process.expect("Checkpoint interval");
    process.send(b"\r\x7f7");
    process.click_label("[Save]", 1);
    process.expect_status("Settings saved");
    let config = std::fs::read_to_string(root.path().join("ede/config.toml")).unwrap();
    assert!(
        config.contains("checkpoint_interval_seconds = 7"),
        "{config}"
    );
    process.send(b"\r\x7fx");
    process.click_label("[Save]", 1);
    process.expect("Enter a whole number from 1 to 3600");
    process.click_label("[Exit]", 1);
    process.expect(">Keep Editing<");
    process.click_label("Save and Exit", 1);
    process.expect("Enter a whole number from 1 to 3600");
    process.resize(20, 4);
    process.expect("Resize to 40x8");
    process.resize(40, 8);
    process.expect("Checkpoint interval (s) x");
    assert_eq!(
        std::fs::read_to_string(root.path().join("ede/config.toml")).unwrap(),
        config
    );
    process.send(b"\x1b");
    process.expect("Checkpoint interval (s) 7");
    process.send(b"\x1b");
    process.expect_absent("Checkpoint interval");
    process.click_label("[Quit]", 1);
    assert!(process.wait().success());
}

/// The first close control targets the inactive tab and leaves the active view in place.
#[test]
fn tab_strip_closes_inactive_tab_by_id() {
    let root = tempfile::tempdir().unwrap();
    let one = root.path().join("one.txt");
    let two = root.path().join("two.txt");
    std::fs::write(&one, "first document").unwrap();
    std::fs::write(&two, "second document").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&one, &two],
        None,
        Some(root.path()),
        None,
    );
    process.expect("one.txt ×");
    process.expect("│");
    process.expect_body("first document", TIMEOUT);
    process.expect_status("Ready");
    process.expect_absent("Use defaults for this session");
    process.send(b"edited ");
    process.expect_body("edited first document", TIMEOUT);
    process.click_label("two.txt", 1);
    process.expect_body("second document", TIMEOUT);
    process.send(b"\x1b[C\x1b[C");
    process.expect_status("Col 3");
    let strip = process.parser.screen().rows(0, 80).next().unwrap();
    let first_close = strip.find('×').unwrap();
    let column = unicode_width::UnicodeWidthStr::width(&strip[..first_close]) + 1;
    process.send(b"\x1bOQ");
    process.expect("Actions (wheel/arrows)");
    process.send(format!("\x1b[<0;{column};1M\x1b[<0;{column};1m").as_bytes());
    process.expect_absent("Actions (wheel/arrows)");
    process.expect("one.txt");
    process.expect_status("Col 3");
    process.click_label("×", 1);
    process.expect("Close —");
    process.expect("one.txt");
    process.send(b"\r");
    process.expect_absent("Save your changes before continuing?");
    process.expect_body("second document", TIMEOUT);
    process.expect_status("Col 3");
    assert_eq!(std::fs::read(&one).unwrap(), b"first document");
    process.click_label("×", 1);
    process.expect("Close —");
    process.send(b"\t\r");
    process.expect_absent("one.txt");
    process.expect_absent("│");
    process.expect_body("second document", TIMEOUT);
    process.expect_status("Col 3");
    assert_eq!(std::fs::read(&one).unwrap(), b"edited first document");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Inactive scratch Save As names the parked tab and leaves the active view at its old caret.
#[test]
fn inactive_scratch_close_names_save_as_target() {
    let root = tempfile::tempdir().unwrap();
    let other = root.path().join("other.txt");
    let destination = root.path().join("saved.txt");
    std::fs::write(&other, "active body").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&other],
        None,
        Some(root.path()),
        None,
    );
    process.expect_status("Ready");
    process.send(b"\x0e");
    process.expect_active_tab("~Untitled 1");
    process.send(b"parked content");
    process.expect_body("parked content", TIMEOUT);
    process.send(b"\x1b[18~");
    process.expect_body("active body", TIMEOUT);
    process.send(b"\x1b[C");
    process.expect_status("Col 2");
    // The first × belongs to the active file. The second belongs to the parked scratch.
    let strip = process.parser.screen().rows(0, 80).next().unwrap();
    let offset = strip.match_indices('×').nth(1).unwrap().0;
    let column = unicode_width::UnicodeWidthStr::width(&strip[..offset]) + 1;
    process.send(format!("\x1b[<0;{column};1M\x1b[<0;{column};1m").as_bytes());
    process.expect("Close — Untitled 1");
    process.send(b"\t\r");
    process.expect("Save As (new path)");
    process.resize(40, 8);
    process.expect("Save Untitled 1");
    process.send(destination.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_absent("Save As (new path)");
    process.expect_body("active body", TIMEOUT);
    process.expect_status("Col 2");
    assert_eq!(std::fs::read(destination).unwrap(), b"parked content");
    assert_eq!(std::fs::read(other).unwrap(), b"active body");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Markdown reaches the real terminal as colored, editable source and honors Plain Text.
#[test]
fn markdown_source_renders_syntax_colors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notes.md");
    std::fs::write(
        &path,
        concat!(
            "# Heading\n## Second\n### Third\n#### Fourth\n##### Fifth\n###### Sixth\n\n",
            "Setext One\n==========\n\nSetext Two\n----------\n\n",
            "- Dash\n+ Plus\n* Star\n\n1. Dot\n\n1) Paren\n\nOrdinary prose\n",
        ),
    )
    .unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("# Heading");
    for (index, color) in [13, 14, 12, 10, 11, 9].into_iter().enumerate() {
        syntax_color(&mut process, index as u16 + 1, 0, vt100::Color::Idx(color));
        syntax_color(
            &mut process,
            index as u16 + 1,
            index as u16 + 2,
            vt100::Color::Idx(color),
        );
    }
    for (row, color) in [(8, 13), (9, 13), (11, 14), (12, 14)] {
        syntax_color(&mut process, row, 0, vt100::Color::Idx(color));
    }
    for (row, marker_len, color) in [
        (14, 1, 11),
        (15, 1, 11),
        (16, 1, 11),
        (18, 2, 14),
        (20, 2, 14),
    ] {
        for column in 0..marker_len {
            syntax_color(&mut process, row, column, vt100::Color::Idx(color));
        }
        syntax_color(&mut process, row, marker_len + 1, vt100::Color::Default);
    }
    process.expect("Ordinary prose");
    syntax_color(&mut process, 22, 0, vt100::Color::Default);
    run_action(&mut process, "Highlighting", 0);
    process.expect("Auto (Markdown)");
    process.send(b"\x1b[B\r");
    process.expect_absent("Auto (Markdown)");
    process.expect("# Heading");
    syntax_color(&mut process, 1, 2, vt100::Color::Default);
    syntax_color(&mut process, 14, 0, vt100::Color::Default);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A mixed Markdown page keeps structural marks, code, and inline meaning distinguishable.
#[test]
fn markdown_mixed_source_uses_distinct_styles() {
    use vt100::Color::{Default, Idx};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("guide.md");
    let lines = [
        "# Reference",
        "1. Ordered step",
        "   - Child bullet",
        "     1) Grandchild number",
        "2. Next step",
        "",
        "- Parent bullet",
        "  1. Child number",
        "     + Grandchild bullet",
        "- [ ] Pending",
        "- [x] Done",
        "",
        "> Plain quote with *light*, **strong**, ~~old~~, and `code`.",
        "> Continuation with `` `literal` `` and \\* escape.",
        "> > Nested quote.",
        ">",
        "> ```rust",
        "> let answer = 42;",
        "> ```",
        "",
        "~~~unknown",
        "# literal > inside fence",
        "~~~",
        "",
        "    - indented literal",
        "",
        "Name | Count",
        ":--- | ---:",
        "`x` | **two**",
        "",
        "[Visible](https://example.test \"title\") and ![Picture](p.png)",
        "[Shortcut] and [Full][Shortcut] and [Shortcut][]",
        "",
        "[Shortcut]: https://example.test/reference \"Ref title\"",
        "",
        "See <https://example.test> <a@example.test> &amp; &#42; and <kbd>Enter</kbd>.",
        "",
        "Break\\",
        "next line",
        "",
        "***",
        "",
        "<div class=\"example\">",
        "<!-- HTML note -->",
        "</div>",
    ];
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("# Reference");
    process.resize(100, 48);
    process.expect("HTML note");
    for (sample, offset, color) in [
        ("1. Ordered", 0, Idx(14)),
        ("- Child bullet", 0, Idx(11)),
        ("1) Grandchild", 1, Idx(14)),
        ("2. Next", 0, Idx(14)),
        ("- Parent bullet", 0, Idx(11)),
        ("1. Child number", 1, Idx(14)),
        ("+ Grandchild", 0, Idx(11)),
        ("[ ] Pending", 1, Idx(11)),
        ("[x] Done", 1, Idx(10)),
        ("Child bullet", 0, Default),
        ("> Plain quote", 0, Idx(12)),
        ("Plain quote", 0, Default),
        ("> Continuation", 0, Idx(12)),
        ("> > Nested", 2, Idx(12)),
        ("*light*", 1, Idx(13)),
        ("**strong**", 2, Idx(11)),
        ("~~old~~", 2, Idx(8)),
        ("`code`", 0, Idx(12)),
        ("`code`", 1, Idx(10)),
        ("`` `literal` ``", 1, Idx(12)),
        ("`` `literal` ``", 3, Idx(10)),
        ("\\* escape", 0, Idx(10)),
        ("```rust", 0, Idx(12)),
        ("```rust", 3, Idx(11)),
        ("let answer", 0, Idx(14)),
        ("answer = 42", 9, Idx(13)),
        ("~~~unknown", 0, Idx(12)),
        ("# literal >", 0, Idx(10)),
        ("- indented literal", 0, Idx(10)),
        ("Name | Count", 0, Idx(13)),
        ("Name | Count", 5, Idx(8)),
        (":--- | ---:", 0, Idx(8)),
        ("`x` | **two**", 4, Idx(8)),
        ("**two**", 2, Idx(11)),
        ("[Visible]", 0, Idx(8)),
        ("[Visible]", 1, Idx(14)),
        ("(https://", 1, Idx(12)),
        ("![Picture]", 2, Idx(14)),
        ("[Full][Shortcut]", 1, Idx(14)),
        ("[Shortcut][]", 1, Idx(14)),
        ("https://example.test/reference", 0, Idx(12)),
        ("<https://", 1, Idx(12)),
        ("<a@example.test>", 1, Idx(12)),
        ("&amp;", 0, Idx(13)),
        ("&#42;", 0, Idx(13)),
        ("<kbd>", 1, Idx(14)),
        ("Enter", 0, Default),
        ("Break\\", 5, Idx(10)),
        ("***", 0, Idx(8)),
        ("<div class", 1, Idx(14)),
        ("class=", 0, Idx(11)),
        ("<!-- HTML note", 0, Idx(8)),
    ] {
        let (row, column) = lines
            .iter()
            .enumerate()
            .find_map(|(row, line)| line.find(sample).map(|column| (row, column)))
            .unwrap();
        syntax_color(
            &mut process,
            row as u16 + 1,
            (column + offset) as u16,
            color,
        );
    }
    syntax_color(&mut process, 19, 2, Idx(12)); // Closing fence in the quote.
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// H03: language detection follows the opened name and changes only on a committed Save As.
#[test]
fn tab_details_language_follows_opened_name_and_committed_save_as() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("actual.txt");
    let opened = directory.path().join("source.RS");
    let busy = directory.path().join("existing.py");
    let saved = directory.path().join("copy.TS");
    std::fs::write(&target, b"source text\n").unwrap();
    std::fs::write(&busy, b"existing\n").unwrap();
    std::os::unix::fs::symlink(&target, &opened).unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&opened));
    process.expect_status("Ready");

    let show_language = |expected, process: &mut PtyChild| {
        run_action(process, "Tabs", 2);
        process.expect(expected);
        process.send(b"\x1b");
        process.expect_absent(expected);
    };
    show_language("Language: Rust (Auto)", &mut process);

    process.send(b"\x1bOS");
    process.expect("Save As (new path)");
    process.send(busy.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect("work retained");
    process.send(b"\x1b");
    process.expect_absent("work retained");
    show_language("Language: Rust (Auto)", &mut process);

    process.send(b"\x1bOS");
    process.expect("Save As (new path)");
    process.send(saved.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_status("Saved");
    show_language("Language: TypeScript (Auto)", &mut process);

    run_action(&mut process, "Highlighting", 0);
    process.expect("Auto (TypeScript)");
    process.send(&[b"\x1b[B".repeat(5).as_slice(), b"\r"].concat());
    show_language("Language: JSON (Manual)", &mut process);
    process.send(b"\x0e");
    process.expect("Untitled 1");
    show_language("Language: Plain Text (Auto)", &mut process);
    process.send(b"\x17");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    process.expect_absent("Untitled 1");
    show_language("Language: JSON (Manual)", &mut process);

    assert_eq!(std::fs::read(&saved).unwrap(), b"source text\n");
    assert_eq!(std::fs::read(&busy).unwrap(), b"existing\n");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Details tracks the first line ending of a parked file across external refreshes.
#[test]
fn tab_details_tracks_parked_file_line_ending() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mixed.txt");
    std::fs::write(&path, b"one\rtwo\r\nthree\nfour\n").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_status("Ready");
    process.send(b"\x0e");
    process.expect("Untitled 1");
    process.expect_status("Ready");
    process.send(b"active scratch");
    process.expect_body("active scratch", TIMEOUT);
    run_action(&mut process, "Tabs", 2);
    process.expect("Line ending: LF");
    process.send(b"\x1b");
    process.expect_absent("Tab Details");

    process.click_label("[Tabs]", 1);
    process.send(b"\x1b[A");
    process.click_label("[Details]", 1);
    process.expect("Line ending: CRLF");
    std::fs::write(&path, b"one\ntwo\r\nthree\r\n").unwrap();
    process.expect("Line ending: LF");
    process.send(b"\x1b");
    process.expect(">Details<");
    process.send(b"\x1b");
    process.expect_absent("Tabs — Enter to switch");
    process.expect_body("active scratch", TIMEOUT);
    process.send(b"\x01\x7f\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Stage 06 must expose a real non-modal search surface through portable input.
#[test]
fn literal_find_panel_uses_unsaved_text() {
    let mut terminal = PtyChild::editor();
    terminal.expect("Ready");
    terminal.send(b"one two one");
    terminal.expect("one two one");
    terminal.send(b"\x06");
    terminal.expect("Find:");
    terminal.send(b"one\r");
    terminal.expect("2 matches");
    terminal.send(b"\x1b");
    terminal.expect_absent("Find:");
    terminal.send(b"\x11");
    terminal.wait();
}

/// Portable Replace dispatch, confirmation, one atomic batch and ordinary Undo.
#[test]
fn literal_replace_all_confirmation_and_undo() {
    let mut terminal = PtyChild::editor();
    terminal.expect("Ready");
    terminal.send(b"one two one");
    terminal.expect("one two one");
    terminal.send(b"\x08");
    terminal.expect("Replace:");
    terminal.send(b"one\t1");
    terminal.click_label("[Replace All]", 1);
    terminal.expect("Replace all — confirm scope");
    terminal.expect(">Cancel<");
    terminal.send(b"\x1b");
    terminal.expect_absent("confirm scope");
    terminal.expect_body("one two one", TIMEOUT);
    terminal.send(b"\r");
    terminal.expect("Replace all — confirm scope");
    terminal.send(b"\x1b[D\r");
    terminal.expect("2 occurrences processed");
    terminal.expect("Replaced ·");
    terminal.send(b"\x1b");
    terminal.expect_absent("Options");
    terminal.expect("1 two 1");
    terminal.send(b"\x1a");
    terminal.expect("one two one");
    terminal.send(b"\x11");
    assert!(terminal.wait().success());
    terminal.assert_restored();
}

/// Test terminal, retained slave attributes, process, and a bounded diagnostic transcript.
struct PtyChild {
    _state_root: Option<tempfile::TempDir>,
    child: Child,
    master: File,
    slave: File,
    original: Termios,
    output: Vec<u8>,
    parser: vt100::Parser,
    keep_alive: bool,
    restored: Option<Termios>,
}

impl PtyChild {
    /// Spawn with a controlling terminal. Unsafe operations run only between fork and exec.
    fn spawn(mut command: Command) -> Self {
        Self::spawn_with_input(&mut command, None)
    }

    /// Attach a separate producer descriptor while keeping the PTY as the controlling tty.
    fn spawn_with_input(command: &mut Command, input: Option<Stdio>) -> Self {
        let state_root = if command
            .get_envs()
            .any(|(name, value)| name == "EDE_STATE_DIR" && value.is_some())
        {
            None
        } else {
            let directory = tempfile::tempdir().unwrap();
            command.env("EDE_STATE_DIR", directory.path().join("state"));
            Some(directory)
        };
        let pty = openpty(
            Some(&Winsize {
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        let slave = File::from(pty.slave);
        let mut original = termios::tcgetattr(&slave).unwrap();
        original.input_flags.insert(InputFlags::IXON);
        termios::tcsetattr(&slave, SetArg::TCSANOW, &original).unwrap();
        command
            .stdin(input.unwrap_or_else(|| Stdio::from(slave.try_clone().unwrap())))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .env_remove("NO_COLOR")
            .env("TERM", "xterm-256color")
            .env("PS1", "EDE_TEST_PROMPT> ");
        // SAFETY: setsid and ioctl are async-signal-safe; fd 1 is the already-duplicated
        // slave PTY. This closure performs no allocation, locking, or Rust-owned mutation.
        unsafe {
            command.pre_exec(|| {
                setsid().map_err(io::Error::from)?;
                if libc::ioctl(1, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        fcntl(&pty.master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();
        Self {
            _state_root: state_root,
            child,
            master: File::from(pty.master),
            slave,
            original,
            output: Vec::new(),
            parser: vt100::Parser::new(
                NonZeroU16::new(24).unwrap(),
                NonZeroU16::new(80).unwrap(),
                0,
            ),
            keep_alive: false,
            restored: None,
        }
    }

    /// Keep the parent shell alive until attributes are captured; macOS revokes a PTY on hangup.
    fn editor() -> Self {
        let mut process = Self::editor_with_term(Some("xterm-256color"));
        process.new_scratch();
        process
    }

    /// Create the scratch needed by a fixture through the regular New command.
    fn new_scratch(&mut self) {
        self.expect_body("New Scratch", TIMEOUT);
        self.send(b"\x0e");
        self.expect_status("Ready");
    }

    /// Read all currently available output without blocking.
    fn drain(&mut self) {
        let mut bytes = [0; 8192];
        loop {
            match self.master.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    self.parser.process(&bytes[..count]);
                    self.output.extend_from_slice(&bytes[..count]);
                    assert!(self.output.len() < 1024 * 1024, "runaway terminal output");
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("PTY read: {error}"),
            }
        }
    }

    /// Wait for actual displayed text; cursor positioning can split words in the raw stream.
    fn expect(&mut self, text: &str) {
        self.expect_with_timeout(text, TIMEOUT);
    }

    /// Wait for the named tab's confirmation, even if its name is already in the strip.
    #[track_caller]
    fn expect_quit_confirmation(&mut self, filename: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            let screen = self.parser.screen().contents();
            if screen
                .lines()
                .any(|row| row.contains("Quit — ") && row.contains(filename))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing Quit confirmation for {filename:?}: {screen}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    /// Larger fixture preparation and long-line seeks have separately measured deadlines.
    fn expect_with_timeout(&mut self, text: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain();
            if self.parser.screen().contents().contains(text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing {text:?}: {}",
                String::from_utf8_lossy(&self.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    /// Wait until an overlay is actually dismissed before sending a dependent key.
    fn expect_absent(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            let screen = self.parser.screen().contents();
            if !screen.contains(text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "still showing {text:?}: {screen}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Observe document rows rather than a matching filename, help label or status message.
    #[track_caller]
    fn expect_body(&mut self, text: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain();
            let height = self.parser.screen().size().0.get();
            if self
                .parser
                .screen()
                .rows(0, 80)
                .skip(1)
                .take(height.saturating_sub(2) as usize)
                .any(|row| row.contains(text))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing document text {text:?}: {}",
                self.parser.screen().contents()
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    /// Wait for plain shell output or a synchronization marker produced after application exit.
    fn expect_raw(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            if self
                .output
                .windows(text.len())
                .any(|window| window == text.as_bytes())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing raw {text:?}: {}",
                String::from_utf8_lossy(&self.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Assert feedback appears in the status row rather than matching the empty document label.
    fn expect_status(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            if self
                .parser
                .screen()
                .rows(0, 80)
                .last()
                .unwrap()
                .contains(text)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing status {text:?}: {}",
                self.parser.screen().contents()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait for the bracketed tab and its active terminal attributes.
    fn expect_active_tab(&mut self, label: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            let screen = self.parser.screen();
            let strip = screen.rows(0, 80).next().unwrap();
            if let Some(start) = strip.find(label)
                && screen
                    .cell(
                        0,
                        unicode_width::UnicodeWidthStr::width(&strip[..start]) as u16,
                    )
                    .is_some_and(|cell| cell.inverse())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "active tab {label:?} missing: {strip}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Separate subsequent assertions from text drawn by earlier frames.
    fn reset_output(&mut self) {
        self.drain();
        self.output.clear();
    }

    /// Send complete SGR button presses over a visible ASCII label.
    fn click_label(&mut self, label: &str, count: usize) {
        self.expect(label);
        let (row, column) = self
            .parser
            .screen()
            .rows(0, 80)
            .enumerate()
            .find_map(|(row, text)| {
                text.find(label).map(|start| {
                    let column = unicode_width::UnicodeWidthStr::width(&text[..start]);
                    (row + 1, column + 1)
                })
            })
            .unwrap();
        let click = format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m");
        self.send(click.repeat(count).as_bytes());
    }

    /// Send actual bytes a conventional terminal would transmit.
    fn send(&mut self, bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut remaining = bytes;
        while !remaining.is_empty() {
            match self.master.write(&remaining[..remaining.len().min(8192)]) {
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.drain();
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => panic!("PTY write: {error}"),
            }
            assert!(Instant::now() < deadline, "terminal input stalled");
        }
    }

    /// The foreground launcher reports its PID before exec; no cross-session ioctl is needed.
    fn editor_pid(&self) -> Pid {
        let text = String::from_utf8_lossy(&self.output);
        let start = text.rfind("EDE_PID:").expect("launcher PID marker") + "EDE_PID:".len();
        let digits: String = text[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        Pid::from_raw(digits.parse().unwrap())
    }
    /// Optional kernel-reported lifetime RSS peak, distinct from live ps samples.
    fn report_peak_rss(&self) {
        if std::env::var_os("EDE_BENCH_PEAK_RSS").is_none() {
            return;
        }
        let output = String::from_utf8_lossy(&self.output);
        let line = output
            .lines()
            .find(|line| {
                line.contains("maximum resident set size")
                    || line.contains("Maximum resident set size (kbytes)")
            })
            .expect("time resource report");
        let value = if cfg!(target_os = "macos") {
            line.split_whitespace()
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                / 1024
        } else {
            line.split(':')
                .next_back()
                .unwrap()
                .trim()
                .parse::<u64>()
                .unwrap()
        };
        eprintln!("kernel_lifetime_peak_editor_RSS_KiB={value}");
    }

    /// Change kernel dimensions and notify the foreground application.
    fn resize(&mut self, width: u16, height: u16) {
        self.parser.set_size(
            NonZeroU16::new(height).unwrap(),
            NonZeroU16::new(width).unwrap(),
        );
        let size = Winsize {
            ws_row: height,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: master is a valid PTY fd and size points to a valid winsize value.
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) },
            0
        );
        // TIOCSWINSZ itself delivers SIGWINCH to the foreground process group.
    }

    /// Wait without risking a stuck CI process, then inspect final restoration.
    fn wait(&mut self) -> ExitStatus {
        if self.keep_alive {
            self.expect_raw("EDE_EXIT:");
            self.restored = Some(termios::tcgetattr(&self.slave).unwrap());
            self.send(b"\n");
            self.keep_alive = false;
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.drain();
            if let Some(status) = self.child.try_wait().unwrap() {
                self.drain();
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "process did not exit: {}",
                String::from_utf8_lossy(&self.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Compare exact captured kernel attributes and output-mode cleanup requests.
    fn assert_restored(&mut self) {
        assert!(
            !self.output.windows(4).any(|window| window == b"\x1b[6n"),
            "application must not query cursor position"
        );
        let restored = self
            .restored
            .clone()
            .unwrap_or_else(|| termios::tcgetattr(&self.slave).unwrap());
        assert_eq!(restored, self.original);
        self.drain();
        for sequence in [
            "\x1b[?1049l",
            "\x1b[?25h",
            "\x1b[?1000l",
            "\x1b[?2004l",
            "\x1b[?1004l",
        ] {
            assert!(
                self.output
                    .windows(sequence.len())
                    .any(|window| window == sequence.as_bytes()),
                "missing restoration sequence {sequence:?}"
            );
        }
    }
}

/// A delayed producer is visible before EOF and the same tab saves exact bytes after EOF.
#[test]
fn piped_input_displays_incrementally_and_becomes_editable_at_eof() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("capture.txt");
    let (mut process, mut writer) = piped_editor();
    writer.write_all(b"\xef").unwrap();
    writer.write_all(b"\xbb\xbflead\r").unwrap();
    process.expect("lead");
    process.send(b"X");
    process.expect_status("Document is read-only");
    writer.write_all(b"\n\xf0\x9f").unwrap();
    writer.write_all(b"\x8c\x8d tail").unwrap();
    drop(writer);
    process.expect("Editable scratch");
    process.expect_absent("Pause View");
    process.send(b"!");
    process.expect("tail!");
    process.send(b"\x13");
    process.expect("Save As");
    process.send(destination.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_status("Saved");
    assert_eq!(
        std::fs::read(destination).unwrap(),
        b"\xef\xbb\xbflead\r\n\xf0\x9f\x8c\x8d tail!"
    );
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

fn piped_editor() -> (PtyChild, File) {
    piped_editor_with_options(&[], None)
}

/// Keep producer bytes on a separate descriptor from keyboard input.
fn piped_editor_with_options(args: &[&str], config: Option<&std::path::Path>) -> (PtyChild, File) {
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};
    let (reader, writer) = nix::unistd::pipe().unwrap();
    for fd in [&reader, &writer] {
        fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).unwrap();
    }
    (
        redirected_editor_with_options(Stdio::from(reader), args, config),
        File::from(writer),
    )
}

fn redirected_editor(input: Stdio) -> PtyChild {
    redirected_editor_with_options(input, &[], None)
}

/// Pass ede's native arguments through the shell wrapper without interpolation.
fn redirected_editor_with_options(
    input: Stdio,
    args: &[&str],
    config: Option<&std::path::Path>,
) -> PtyChild {
    let mut command = Command::new("/bin/sh");
    // Drop the wrapper's pipe reader after launching EDE. Keep only its tty open
    // so Darwin allows the parent test to inspect termios after EDE exits.
    command.args([
        "-c",
        r#"
exec 3<&0 0</dev/tty
"$@" <&3 3<&- &
pid=$!
printf 'EDE_PID:%s\n' "$pid"
exec 3<&-
wait "$pid"
result=$?
printf '\nEDE_EXIT:%s\n' "$result"
read -r _completion
exit "$result"
"#,
        "ede-pipe-test",
        env!("CARGO_BIN_EXE_ede"),
    ]);
    command.args(args);
    let config_root = tempfile::tempdir().unwrap();
    command.env("XDG_CONFIG_HOME", config.unwrap_or(config_root.path()));
    let mut process = PtyChild::spawn_with_input(&mut command, Some(input));
    process.keep_alive = true;
    process
}

/// K06: Safe startup preserves stdin promotion and the normal recovery checkpoint.
#[test]
fn safe_config_stdin_becomes_recoverable_scratch() {
    let (mut process, mut producer) = piped_editor_with_options(&["--safe-config", "--"], None);
    producer.write_all(b"safe stdin input").unwrap();
    process.expect_body("safe stdin input", TIMEOUT);
    drop(producer);
    process.expect_status("Editable scratch");
    process.send(b"!");
    process.expect_body("safe stdin input!", TIMEOUT);
    let state = process._state_root.as_ref().unwrap().path().join("state");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();

    let mut reopened =
        PtyChild::editor_with_state(Some("xterm-256color"), ["--safe-config"], Some(&state));
    reopened.expect_body("safe stdin input!", TIMEOUT);
    reopened.send(b"\x11");
    assert!(reopened.wait().success());
    reopened.assert_restored();
}

/// K07: a live reload reaches a stdin scratch even when the producer closes concurrently.
#[test]
fn settings_checkpoint_interval_reaches_promoted_stdin() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("ede/config.toml");
    std::fs::create_dir(config.parent().unwrap()).unwrap();
    let configure = |seconds| {
        std::fs::write(
            &config,
            format!("[recovery]\ncheckpoint_interval_seconds = {seconds}"),
        )
        .unwrap()
    };
    configure(30);
    let (mut process, mut producer) = piped_editor_with_options(&[], Some(root.path()));
    producer.write_all(b"cadence applies to stdin").unwrap();
    process.expect_body("cadence applies to stdin", TIMEOUT);
    process.expect_status("User config");
    configure(1);
    run_action(&mut process, "Settings", 1);
    let started = Instant::now();
    drop(producer);
    process.expect_status("Editable scratch");
    process.send(b"!");
    process.expect_body("cadence applies to stdin!", TIMEOUT);
    process.expect_status("Scratch checkpoint retained");
    assert!(started.elapsed() < Duration::from_secs(4));
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

#[test]
fn redirected_file_stdin_reads_from_inherited_position_and_escapes_controls() {
    use std::io::{Seek, SeekFrom};
    let input = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(input.path(), b"ignored\nvisible-\x1b[31m-text").unwrap();
    let mut file = input.reopen().unwrap();
    file.seek(SeekFrom::Start(8)).unwrap();
    let mut process = redirected_editor(Stdio::from(file));
    process.expect("visible-<U+001B>[31m-text");
    assert!(!process.parser.screen().contents().contains("ignored"));
    process.expect_status("Editable scratch");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

#[test]
fn pipe_reports_hangup_and_releases_idle_producer() {
    let (mut process, mut writer) = piped_editor();
    writer.write_all(b"idle stream marker").unwrap();
    process.expect("idle stream marker");
    nix::sys::signal::kill(process.editor_pid(), nix::sys::signal::Signal::SIGHUP).unwrap();
    assert_eq!(process.wait().code(), Some(129));
    process.assert_restored();
    assert_eq!(
        writer.write(b"after hangup").unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
}

/// Retained Find continues to the next occurrence even when appends stale its decorations.
#[test]
fn live_find_advances_after_append() {
    let (mut process, mut writer) = piped_editor();
    writer.write_all(b"stream item one\n").unwrap();
    process.expect("stream item one");
    process.send(b"\x06");
    process.expect("Find:");
    process.send(b"item\r");
    process.expect("1 matches");
    process.expect_status("Ln 1 Col 8");
    writer.write_all(b"stream item two\n").unwrap();
    process.expect("Results stale");
    process.click_label("[Next]", 1);
    process.expect_status("Ln 2 Col 8");
    drop(writer);
    process.expect_status("Editable scratch");
    process.send(b"\x1b");
    process.expect_absent("Find:");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Stage 08 release probe with a gated producer and a companion editable tab.
#[test]
#[ignore = "1 GiB retained stream; run explicitly with EDE_BENCH_CORPUS=ordinary|dense|long"]
fn sustained_piped_capture_benchmark() {
    use std::sync::mpsc;

    let mib = std::env::var("EDE_BENCH_MIB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1024);
    let corpus = std::env::var("EDE_BENCH_CORPUS").unwrap_or_else(|_| "ordinary".into());
    let launch = Instant::now();
    let (mut process, mut writer) = piped_editor();
    writer.write_all(b"first stream marker").unwrap();
    process.expect_body("first stream marker", TIMEOUT);
    let pid = process.editor_pid();
    let first_text = launch.elapsed();
    let (allow, proceed) = mpsc::sync_channel(0);
    let producer = thread::spawn(move || {
        let mut block = [b'x'; 64 << 10];
        let stride = match corpus.as_str() {
            "ordinary" => 128,
            "dense" => 2,
            "long" => block.len() + 1,
            _ => panic!("unknown corpus"),
        };
        for line in block.chunks_exact_mut(stride) {
            *line.last_mut().unwrap() = b'\n';
        }
        proceed.recv().unwrap();
        for _ in 0..mib * 16 {
            writer.write_all(&block).unwrap();
        }
        writer.write_all(b"last stream marker").unwrap();
        writer
    });
    let sustained = Instant::now();
    allow.send(()).unwrap();
    process.send(b"\x0e");
    process.expect_status("Ready");
    let mut max_input = Duration::ZERO;
    for (index, letter) in b"companion".iter().enumerate() {
        let began = Instant::now();
        process.send(&[*letter]);
        process.expect_body(
            std::str::from_utf8(&b"companion"[..=index]).unwrap(),
            TIMEOUT,
        );
        max_input = max_input.max(began.elapsed());
    }
    process.send(b"\x1b[18~");
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut sampled = Instant::now();
    let mut peak_rss = 0;
    let mut peak_fds = 0;
    while !producer.is_finished() {
        process.reset_output();
        if sampled.elapsed() >= Duration::from_millis(100) {
            sampled = Instant::now();
            let output = Command::new("ps")
                .args(["-o", "rss=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            if let Ok(rss) = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<u64>()
            {
                peak_rss = peak_rss.max(rss);
            }
            if let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) {
                peak_fds = peak_fds.max(fds.count());
            }
        }
        assert!(
            Instant::now() < deadline,
            "producer blocked for more than three minutes"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let writer = producer.join().unwrap();
    process.expect_body("last stream marker", Duration::from_secs(60));
    let transferred = sustained.elapsed();
    process.send(b"\x06");
    process.expect("Find:");
    process.send(b"last stream marker\r");
    process.expect_body("1 matches", Duration::from_secs(120));
    let eof = Instant::now();
    drop(writer);
    process.expect_status("Editable scratch");
    let adopted = eof.elapsed();
    eprintln!(
        "stream_MiB={mib} first_text_ms={:.3} throughput_MiB_s={:.2} max_companion_input_ms={:.3} eof_adoption_ms={:.3} sampled_rss_KiB={peak_rss} proc_fd_high_water={peak_fds}",
        first_text.as_secs_f64() * 1000.0,
        mib as f64 / transferred.as_secs_f64(),
        max_input.as_secs_f64() * 1000.0,
        adopted.as_secs_f64() * 1000.0
    );
    // Bench state is disposable. Avoid timing the separate recovery checkpoint on exit.
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).unwrap();
    assert_eq!(process.wait().code(), Some(143));
    process.assert_restored();
}

/// Cancel keeps the idle source available. Confirmed Close promptly drops the read end.
#[test]
fn idle_pipe_close_confirms_and_releases_producer() {
    let (mut process, mut writer) = piped_editor();
    writer.write_all(b"first marker\n").unwrap();
    process.expect("first marker");
    process.send(b"\x17");
    process.expect("discard retained stream history");
    process.send(b"\x1b");
    process.expect_absent("discard retained stream history");
    writer.write_all(b"after cancel\n").unwrap();
    process.expect("after cancel");
    process.send(b"\x17");
    process.expect("discard retained stream history");
    process.send(b"\x1b[Z\r");
    process.expect("No open tabs");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match writer.write(b"x") {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
            Ok(1) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            result => panic!("closed stream did not release its reader: {result:?}"),
        }
    }
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Pathname rotation retains the old generation and never joins separate partial lines.
#[test]
fn pathname_follow_keeps_old_text_and_separates_rotated_text() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("current.log");
    let old = directory.path().join("old.log");
    std::fs::write(&path, b"old line\npartial\r").unwrap();
    let mut process = PtyChild::editor_with_args(
        Some("xterm-256color"),
        [std::ffi::OsStr::new("--follow"), path.as_os_str()],
    );
    process.expect("partial");
    process.expect_status("Waiting for more");
    std::fs::rename(&path, &old).unwrap();
    std::fs::write(&path, b"\xef\xbb\xbfnew generation\n").unwrap();
    process.expect("new generation");
    process.expect("partial<U+000D>");
    assert_eq!(std::fs::read(&old).unwrap(), b"old line\npartial\r");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"\xef\xbb\xbfnew generation\n"
    );
    process.send(b"\x11");
    process.expect("discard retained stream history");
    process.send(b"\x1b[Z\r");
    assert!(process.wait().success());
    process.assert_restored();
}

/// K06: A following tab is still read-only and retains its Quit confirmation.
#[test]
fn safe_config_follow_uses_ordinary_stream_guards() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("history.log");
    let bytes = b"retained history\n";
    std::fs::write(&source, bytes).unwrap();
    let mut process = PtyChild::editor_with_args(
        Some("xterm-256color"),
        [
            std::ffi::OsStr::new("--safe-config"),
            std::ffi::OsStr::new("--follow"),
            source.as_os_str(),
        ],
    );
    process.expect_body("retained history", TIMEOUT);
    process.send(b"X");
    process.expect_status("Document is read-only");
    process.send(b"\x11");
    process.expect("discard retained stream history");
    process.send(b"\x1b");
    process.expect_absent("discard retained stream history");
    process.send(b"\x11");
    process.expect("discard retained stream history");
    process.send(b"\x1b[Z\r");
    assert!(process.wait().success());
    process.assert_restored();
    assert_eq!(std::fs::read(source).unwrap(), bytes);
}

/// K06: with --, a leading dash is part of the filename even in safe mode.
#[test]
fn safe_config_opens_literal_dash_prefixed_path() {
    let mut source = tempfile::NamedTempFile::with_prefix_in("-ede-dash-", ".").unwrap();
    source.write_all(b"dash path contents").unwrap();
    let mut process = PtyChild::editor_with_args(
        Some("xterm-256color"),
        [
            std::ffi::OsStr::new("--safe-config"),
            std::ffi::OsStr::new("--"),
            source.path().file_name().unwrap(),
        ],
    );
    process.expect_body("dash path contents", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
    assert_eq!(std::fs::read(source.path()).unwrap(), b"dash path contents");
}

/// K02: apply bindings at dispatch and preserve document-save and dirty-Quit guards.
#[test]
fn general_config_changes_save_quit_and_search_focus_routes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("ede");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("config.toml"),
        "[keybindings.global]\nsave = ['F9']\nquit = ['Ctrl+B']\n\
         [keybindings.editor]\nfocus_search = ['F10']\nmove_right = ['Alt+Right']\nselect_right = ['F12']\n\
         [keybindings.search]\nfocus_editor = ['F11']\nfield_backspace = ['Ctrl+D']\n",
    )
    .unwrap();
    let source = directory.path().join("source.txt");
    std::fs::write(&source, b"original").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&source],
        None,
        Some(directory.path()),
        None,
    );
    process.expect_status("User config");
    process.expect_status("Ready");
    process.expect_body("original", TIMEOUT);
    process.send(b"\x06oriX\x04g");
    process.expect(">Find: orig");
    process.send(b"\x1b[23~");
    process.send(b"X");
    process.expect_body("Xoriginal", TIMEOUT);
    process.send(b"\x1b[21~");
    process.expect(">Find: orig");
    process.send(b"\x1b");
    process.expect_absent("Find: orig");
    process.send(b"\x1b[1;3C\x1b[24~R");
    process.expect_body("XoRiginal", TIMEOUT);
    process.send(b"\x13");
    assert_eq!(std::fs::read(&source).unwrap(), b"original");
    process.send(b"\x1b[20~");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(&source).unwrap(), b"XoRiginal");
    process.send(b"Y\x11\x02");
    process.expect("Save your changes before continuing?");
    process.send(b"\x1b[Z\r");
    assert!(process.wait().success());
    process.assert_restored();
    assert_eq!(std::fs::read(source).unwrap(), b"XoRiginal");
}

/// K01/K06: an invalid reload retains the last map, and a bad startup stays recoverable.
#[test]
fn invalid_general_config_preserves_previous_revision_and_default_quit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("ede");
    std::fs::create_dir(&root).unwrap();
    let config = root.join("config.toml");
    std::fs::write(&config, "[keybindings.global]\nquit = ['Ctrl+B']").unwrap();
    let source = directory.path().join("untouched.txt");
    std::fs::write(&source, "original").unwrap();
    let start = || {
        PtyChild::editor_with_options(
            Some("xterm-256color"),
            [&source],
            None,
            Some(directory.path()),
            None,
        )
    };
    let mut running = start();
    running.expect_status("User config");
    running.expect_status("Ready");
    std::fs::write(&config, "[keybindings.global]\nquit = ['Cmd+Q']").unwrap();
    run_action(&mut running, "Settings", 1);
    running.expect_status("Settings error");
    running.resize(20, 4);
    running.expect("Ctrl+B Quit");
    running.send(b"\x02");
    assert!(running.wait().success());
    running.assert_restored();

    let mut fallback = start();
    fallback.expect_status("Settings error");
    fallback.expect_status("Ready");
    run_action(&mut fallback, "Settings", 2); // Pause mouse capture.
    fallback.send(b"X");
    fallback.expect_body("Xoriginal", TIMEOUT);
    fallback.send(b"\x11");
    fallback.expect("Save your changes before continuing?");
    fallback.send(b"\x1b[Z\r");
    assert!(fallback.wait().success());
    fallback.assert_restored();
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "[keybindings.global]\nquit = ['Cmd+Q']"
    );
    assert_eq!(std::fs::read_to_string(source).unwrap(), "original");
}

/// K07: an offscreen byte limit applies to the exact text and a reload rechecks eligibility.
#[test]
fn general_highlighting_limits_recheck_offscreen_crlf_lines() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("ede");
    std::fs::create_dir(&root).unwrap();
    let config = root.join("config.toml");
    let source = directory.path().join("limits.rs");
    let content = format!(
        "fn example() {{}}\r\n{}{}\r\n",
        "\r\n".repeat(40),
        "x".repeat(32)
    );
    std::fs::write(&source, format!("\u{feff}{content}")).unwrap();
    let write_limits = |line, document| {
        std::fs::write(
            &config,
            format!("[highlighting]\nmax_line_bytes = {line}\nmax_document_bytes = {document}\n"),
        )
        .unwrap();
    };
    write_limits(31, content.len());
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&source],
        None,
        Some(directory.path()),
        None,
    );
    process.expect_status("Plain syntax");
    write_limits(32, content.len());
    run_action(&mut process, "Settings", 1);
    syntax_color(&mut process, 1, 0, vt100::Color::Idx(14));
    write_limits(0, content.len());
    run_action(&mut process, "Settings", 1);
    process.expect_status("Settings error");
    syntax_color(&mut process, 1, 0, vt100::Color::Idx(14));
    std::fs::write(
        root.join("highlighting.toml"),
        "default_theme = 'Aqua'\n[themes.Aqua]\nbase = 'dark'\n[themes.Aqua.colors]\nkeyword = '#1188DD'\n",
    ).unwrap();
    run_action(&mut process, "Highlighting", 2);
    syntax_color(&mut process, 1, 0, vt100::Color::Rgb(0x11, 0x88, 0xdd));
    process.expect_status("Settings error");
    write_limits(32, content.len() - 1);
    run_action(&mut process, "Settings", 1);
    process.expect_status("Plain syntax");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// K04/K05: edit a scalar with the keyboard at the minimum size and persist it.
#[test]
fn settings_keyboard_save_round_trips_at_minimum_size() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("ede");
    std::fs::create_dir(&root).unwrap();
    let config = root.join("config.toml");
    std::fs::write(
        &config,
        "# Keep this comment\n[keybindings.global]\nsettings = ['F9'] # keyboard opener\n",
    )
    .unwrap();
    let start = || {
        PtyChild::editor_with_options(
            Some("xterm-256color"),
            std::iter::empty::<&str>(),
            None,
            Some(directory.path()),
            None,
        )
    };
    let mut process = start();
    process.expect_status("User config");
    process.resize(40, 8);
    process.send(b"\x1b[20~");
    process.expect("Checkpoint interval (s) 5");
    process.send(b"\r");
    process.expect("Range 1–3600");
    process.send(b"\x7f2\r");
    process.expect("Checkpoint interval (s) 2");
    process.send(b"\t\r");
    process.expect_status("Settings saved");
    process.send(b"\x1b");
    process.expect_absent("Checkpoint interval");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();

    let output = std::fs::read_to_string(&config).unwrap();
    assert!(output.contains("checkpoint_interval_seconds = 2"));
    assert!(output.contains("# Keep this comment"));
    assert!(output.contains("settings = ['F9'] # keyboard opener"));
    let mut restarted = start();
    restarted.expect_status("User config");
    restarted.send(b"\x1b[20~");
    restarted.expect("Checkpoint interval (s) 2");
    restarted.click_label("Restore Defaults", 1);
    restarted.expect("Checkpoint interval (s) 5");
    assert!(
        std::fs::read_to_string(&config)
            .unwrap()
            .contains("checkpoint_interval_seconds = 2")
    );
    restarted.send(b"\t\r");
    restarted.expect_status("Settings saved");
    restarted.send(b"\x1b");
    restarted.expect_absent("Checkpoint interval");
    restarted.send(b"\x11");
    assert!(restarted.wait().success());
    restarted.assert_restored();
    let output = std::fs::read_to_string(&config).unwrap();
    assert!(!output.contains("checkpoint_interval_seconds ="));
    assert!(!output.contains("settings ="));
    assert!(output.contains("# keyboard opener"));
}

/// K04: mouse entry, captured Enter, resize/cancel, typed replacement and discard protection.
#[test]
fn settings_capture_and_dirty_confirmation_at_minimum_size() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("ede");
    std::fs::create_dir(&root).unwrap();
    let config = root.join("config.toml");
    let original = "[keybindings.global]\nsettings = ['F9']\n";
    std::fs::write(&config, original).unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        None,
        Some(directory.path()),
        None,
    );
    process.expect_status("User config");
    process.resize(40, 8);
    process.send(b"\x1b[20~");
    process.expect("Checkpoint interval (s)");
    process.click_label("[Keys]", 1);
    process.send(b"\t\x1b[C\x1b[C\x1b[B");
    process.click_label("Activate button", 1);
    process.expect("1/1 Enter (Default)");
    process.click_label("[Replace]", 1);
    process.expect("Type the replacement shortcut");
    process.click_label("[Capture]", 1);
    process.expect("Press a shortcut to preview");
    process.send(b"\r\r\r");
    process.expect("Captured shortcut");
    process.expect(">Discard<");
    process.send(b"\x1b");
    process.expect("Type the replacement shortcut");
    process.click_label("[Capture]", 1);
    process.expect("Press a shortcut to preview");
    process.resize(20, 4);
    process.expect("Escape exit capture");
    process.send(b"\x1b");
    process.expect_absent("exit capture");
    process.resize(40, 8);
    process.expect("Type the replacement shortcut");
    process.send(b"\x7f\x7f\x7f\x7f\x7f\x1b[200~F10\x1b[201~\r");
    process.expect("1/1 F10 (Custom)");
    process.send(b"\x1b");
    process.expect("Activate button");
    process.send(b"\x1b");
    process.expect(">Keep Editing<");
    process.resize(20, 4);
    process.expect("Escape Cancel");
    process.resize(40, 8);
    process.expect(">Keep Editing<");
    process.send(b"\r");
    process.expect("Activate button");
    process.send(b"\x1b");
    process.expect(">Keep Editing<");
    process.send(b"\t\t\r");
    process.expect_absent("Keep Editing");
    assert_eq!(std::fs::read_to_string(config).unwrap(), original);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

impl Drop for PtyChild {
    /// Terminate the owned child and give it a bounded chance to reap.
    fn drop(&mut self) {
        let _ = self.child.kill();
        // A killed Darwin session leader can remain in tty revocation until
        // this fixture's slave handle closes. Never block a failed test here.
        let deadline = Instant::now() + Duration::from_millis(150);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Ctrl+S cannot freeze output, Ctrl+C is copy, and pasted control bytes never become commands.
#[test]
fn real_shortcuts_paste_and_cleanup() {
    let mut process = PtyChild::editor();
    process.expect("Ready");
    assert!(
        !termios::tcgetattr(&process.slave)
            .unwrap()
            .input_flags
            .contains(InputFlags::IXON)
    );
    process.send(b"\x13");
    process.expect("Save As (new path)");
    process.send(b"\x1b");
    thread::sleep(Duration::from_millis(60));
    process.send(b"\x03");
    process.expect_status("No selection");
    process.send(b"\x1b[200~\x11\x13secret\x1b[201~");
    process.expect("secret");
    process.expect("<U+0011>");
    assert!(process.child.try_wait().unwrap().is_none());
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Workspace controls have distinct colors, noninteractive gaps, and visible keyboard focus.
#[test]
fn workspace_header_buttons_are_spaced_colored_and_reachable() {
    let header = "[Tabs]  [Settings]  [Menu]  [Quit]";
    for (width, height) in [(80, 24), (40, 8)] {
        let mut process = PtyChild::editor();
        if width != 80 {
            process.resize(width, height);
            process.expect_status("Ready");
        }
        let start = width - header.len() as u16;
        let row = process.parser.screen().rows(0, width).next().unwrap();
        let Some(strip) = row.strip_suffix(header) else {
            panic!("missing right-aligned header at {width} columns: {row}");
        };
        assert!(
            strip.contains('×'),
            "active tab lost its close button: {row}"
        );
        assert!(process.parser.screen().cell(0, 0).unwrap().inverse());
        for (label, color) in [
            ("[Tabs]", 6),
            ("[Settings]", 3),
            ("[Menu]", 5),
            ("[Quit]", 1),
        ] {
            let x = start + header.find(label).unwrap() as u16;
            assert_eq!(
                process.parser.screen().cell(0, x).unwrap().fgcolor(),
                vt100::Color::Idx(color),
                "{label} at {width} columns"
            );
        }

        for (offset, character) in header.bytes().enumerate() {
            if character == b' ' {
                let column = usize::from(start) + offset + 1;
                process.send(format!("\x1b[<0;{column};1M\x1b[<0;{column};1m").as_bytes());
                process.send(b"\x1b[17~"); // F6 moves focus from the editor to Tabs.
                process.expect(">Tabs<");
                let focus = process.parser.screen().cell(0, start).unwrap();
                assert!(focus.bold());
                assert_eq!(focus.fgcolor(), vt100::Color::Idx(6));
                process.send(b"\x1b[Z"); // Shift+Tab returns focus to the editor.
                process.expect_absent(">Tabs<");
            }
        }
        process.click_label("[Tabs]", 1);
        process.expect("Tabs — Enter to switch");
        process.send(b"\x1b");
        process.click_label("[Settings]", 1);
        process.expect("Checkpoint interval");
        process.send(b"\x1b");
        process.click_label("[Menu]", 1);
        process.expect("Actions (wheel/arrows)");
        process.send(b"\x1b[F\x1b[C\r"); // Last category, first command.
        process.expect("scroll help");
        process.send(b"\x1b");
        process.expect_absent("scroll help");
        process.send(b"\x1bOP"); // F1.
        process.expect("scroll help");
        process.send(b"\x1b");
        process.expect(">Menu<");
        process.send(b"\t");
        process.click_label(">Quit<", 1);
        assert!(process.wait().success());
        process.assert_restored();
    }
}

/// Help is modal and mouse transitions use the rendered target rectangles after resize.
#[test]
fn real_mouse_overlay_and_resize() {
    let mut process = PtyChild::editor();
    process.expect("Ready");
    let header = process.parser.screen().rows(0, 80).next().unwrap();
    let quit = header.rfind("[Quit]").unwrap();
    let column = unicode_width::UnicodeWidthStr::width(&header[..quit]) + 1;
    process.reset_output();
    process.send(b"\x1bOP");
    process.expect("Cmd aliases require");
    // Header Quit is underneath the overlay and must not be activated.
    process.send(format!("\x1b[<0;{column};1M\x1b[<0;{column};1m").as_bytes());
    process.expect_absent("Cmd aliases require");
    assert!(process.child.try_wait().unwrap().is_none());
    process.send(b"\x1bOP");
    process.expect("Cmd aliases require");
    assert!(process.child.try_wait().unwrap().is_none());
    process.reset_output();
    process.resize(20, 4);
    process.expect("Resize");
    process.reset_output();
    process.resize(80, 24);
    process.expect("Cmd aliases require");
    process.send(b"\x1b");
    process.click_label("[Quit]", 1);
    assert!(process.wait().success());
    process.assert_restored();
}

/// External termination remains distinct from Ctrl+C, restores all modes, and reports its signal.
#[test]
fn external_signals_restore_terminal() {
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        let mut process = PtyChild::editor();
        process.expect("Ready");
        kill(process.editor_pid(), signal).unwrap();
        assert_eq!(process.wait().code(), Some(128 + signal as i32));
        process.assert_restored();
    }
}

/// Startup rejects unusable TERM before terminal mutation even with actual TTY descriptors.
#[test]
fn unusable_term_is_a_plain_error() {
    for term in [None, Some("dumb"), Some("")] {
        let mut process = PtyChild::editor_with_term(term);
        assert_eq!(process.wait().code(), Some(1));
        assert!(!process.output.contains(&0x1b));
        assert_eq!(process.restored.as_ref().unwrap(), &process.original);
    }
}

impl PtyChild {
    /// Override TERM after the harness sets up TTY handles, testing the actual child environment.
    fn editor_with_term(term: Option<&str>) -> Self {
        Self::editor_with_path(term, None)
    }
    /// Supply an optional OS-native path through positional shell arguments, without interpolation.
    fn editor_with_path(term: Option<&str>, path: Option<&std::path::Path>) -> Self {
        Self::editor_with_args(term, path)
    }

    /// Supply the original native arguments to the executable under job control.
    fn editor_with_args(
        term: Option<&str>,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
    ) -> Self {
        Self::editor_with_state(term, args, None)
    }

    /// Share only the selected recovery profile across otherwise independent processes.
    fn editor_with_state(
        term: Option<&str>,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
        state: Option<&std::path::Path>,
    ) -> Self {
        Self::editor_with_options(term, args, state, None, None)
    }

    /// Start the editor with isolated state, highlighting configuration and optional soft limit.
    fn editor_with_options(
        term: Option<&str>,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
        state: Option<&std::path::Path>,
        config: Option<&std::path::Path>,
        descriptor_limit: Option<u16>,
    ) -> Self {
        // The interactive parent supplies job control and stays alive after ede exits,
        // allowing exact post-exit termios capture before Darwin revokes the slave PTY.
        let mut command = Command::new("/bin/sh");
        let script = r#"
if [ "$1" = unset ]; then unset TERM; else TERM=$2; export TERM; fi
if [ -n "$3" ]; then ulimit -Sn "$3" || exit 1; fi
shift 3
# The nested shell expands its own PID and positional parameters.
# shellcheck disable=SC2016
/bin/sh -c 'printf "EDE_PID:%s\n" "$$"; program=$1; shift; exec "$program" "$@"' ede-child "$@"
result=$?
printf '\nEDE_EXIT:%s\n' "$result"
read -r _completion
exit "$result"
"#;
        let script = if std::env::var_os("EDE_BENCH_PEAK_RSS").is_some() {
            script.replacen(
                "\n/bin/sh -c",
                if cfg!(target_os = "macos") {
                    "\n/usr/bin/time -l /bin/sh -c"
                } else {
                    "\n/usr/bin/time -v /bin/sh -c"
                },
                1,
            )
        } else {
            script.into()
        };
        let descriptor_limit = descriptor_limit.map(|limit| limit.to_string());
        command.args([
            "-i",
            "-c",
            script.as_str(),
            "ede-term-test",
            if term.is_none() { "unset" } else { "set" },
            term.unwrap_or(""),
            descriptor_limit.as_deref().unwrap_or(""),
            env!("CARGO_BIN_EXE_ede"),
        ]);
        command.args(args);
        if let Some(state) = state {
            command.env("EDE_STATE_DIR", state);
        }
        let config_root = tempfile::tempdir().unwrap();
        command.env("XDG_CONFIG_HOME", config.unwrap_or(config_root.path()));
        let mut child = Self::spawn(command);
        child.keep_alive = true;
        child
    }
}

/// R01: Quit retains scratch text without asking for a filename or discard decision.
#[test]
fn scratch_quit_and_relaunch_retains_text_without_save_prompts() {
    let state = tempfile::tempdir().unwrap();
    let profile = state.path().join("state");
    let mut first = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    first.new_scratch();
    first.send("retained scratch 界e\u{301}".as_bytes());
    first.expect_body("retained scratch", TIMEOUT);
    first.send(b"\x11");
    assert!(first.wait().success());
    first.assert_restored();
    let mut second = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    second.expect_body("retained scratch 界e\u{301}", TIMEOUT);
    second.send(b"\x1a");
    second.expect_body("retained scratch 界e\u{301}", TIMEOUT);
    second.send(b"\x11");
    assert!(second.wait().success());
    second.assert_restored();
}

/// Replacement revisions participate in normal-exit scratch retention.
#[test]
fn replaced_scratch_is_restored_after_normal_quit() {
    let state = tempfile::tempdir().unwrap();
    let profile = state.path().join("state");
    let mut first = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    first.new_scratch();
    first.send(b"one two one\x08one\t1");
    first.click_label("[Replace All]", 1);
    first.expect("Replace all — confirm scope");
    first.send(b"\t\r");
    first.expect("2 occurrences processed");
    first.send(b"\x1b");
    first.expect_absent("Options");
    first.send(b"\x11");
    assert!(first.wait().success());
    first.assert_restored();
    let mut second = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    second.expect_body("1 two 1", TIMEOUT);
    second.send(b"\x11");
    assert!(second.wait().success());
    second.assert_restored();
}

/// F01: An external named-file save refreshes the displayed text without terminal input.
#[test]
fn clean_named_file_refreshes_without_user_input() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("refresh.txt");
    std::fs::write(&path, "original disk contents").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_body("original disk contents", TIMEOUT);
    process.expect("Ready");
    std::fs::write(&path, "updated by another process").unwrap();
    process.expect_body("updated by another process", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A valid Linux XDG state root does not depend on HOME being set.
#[test]
#[cfg(target_os = "linux")]
fn xdg_state_root_works_without_home() {
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        "unset HOME EDE_STATE_DIR; exec \"$1\"",
        "ede-xdg",
        env!("CARGO_BIN_EXE_ede"),
    ]);
    command.env("XDG_STATE_HOME", directory.path());
    let mut process = PtyChild::spawn(command);
    process.new_scratch();
    process.send(b"\x11");
    assert!(process.wait().success());
    assert!(directory.path().join("ede/catalog").exists());
}

/// F01/F04: symlink retargets, rename saves and transient invalid sources preserve last-good text.
#[test]
fn refresh_recovers_after_missing_invalid_and_retargeted_sources() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("watched");
    std::fs::write(&path, "last good text").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_body("last good text", TIMEOUT);
    process.expect_status("Ready");
    std::fs::remove_file(&path).unwrap();
    process.expect("File refresh unavailable");
    process.expect_body("last good text", TIMEOUT);
    std::fs::write(&path, b"\xffinvalid").unwrap();
    process.expect("File refresh failed");
    process.expect_body("last good text", TIMEOUT);
    let target = directory.path().join("target");
    std::fs::write(&target, "valid symlink target").unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink(&target, &path).unwrap();
    process.expect_body("valid symlink target", TIMEOUT);
    let replacement = directory.path().join("replacement");
    std::fs::write(&replacement, "rename saved").unwrap();
    std::fs::rename(&replacement, &target).unwrap();
    process.expect_body("rename saved", TIMEOUT);
    process.send(b"local ");
    process.expect_body("local rename saved", TIMEOUT);
    std::fs::write(&target, "external while dirty").unwrap();
    process.expect("local edits preserved");
    process.expect_body("local rename saved", TIMEOUT);
    process.send(b"\x13");
    process.expect("work retained");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "external while dirty"
    );
    process.send(b"\x1b");
    process.expect_absent("work retained");
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
}

/// R04/R05/I01: retired profiles combine, while excess scratches stay operable through Recovery.
#[test]
fn independent_instances_restore_overflow_and_open_after_freeing_a_slot() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("state");
    let launch = || {
        PtyChild::editor_with_state(
            Some("xterm-256color"),
            std::iter::empty::<&str>(),
            Some(&profile),
        )
    };
    let mut a = launch();
    a.new_scratch();
    let mut b = launch();
    b.new_scratch();
    a.send(&[0x0e; 32]);
    a.expect("Untitled 33");
    b.send(&[0x0e; 32]);
    b.expect("Untitled 33");
    a.send(b"\x11");
    assert!(a.wait().success());
    b.send(b"\x11");
    assert!(b.wait().success());
    let mut c = launch();
    c.expect_with_timeout("Untitled 33", Duration::from_secs(15));
    c.expect_status("Ready");
    run_action(&mut c, "File", 4);
    c.expect("Recovery — retained scratches");
    c.expect("Scratch 65");
    c.expect("Scratch 66");
    c.send(b"\r");
    c.expect("64 open tabs");
    c.send(b"\x1b");
    c.expect_absent("Details");
    c.send(b"\x17");
    c.expect("Unsaved changes");
    c.send(b"\t\t\r");
    c.expect_active_tab("~Untitled 34");
    run_action(&mut c, "File", 4);
    c.expect("Scratch 65");
    c.send(b"\r");
    c.expect_active_tab("~Untitled 65");
    c.send(b"\x11");
    assert!(c.wait().success());
    c.assert_restored();
}

/// Startup order, keyboard tab switching, and tab-local edit history reach the executable.
#[test]
fn startup_tabs_keep_independent_contents_and_history() {
    let directory = tempfile::tempdir().unwrap();
    let a = directory.path().join("alpha.txt");
    let b = directory.path().join("beta.txt");
    std::fs::write(&a, "alpha body").unwrap();
    std::fs::write(&b, "beta body").unwrap();
    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), [&a, &b, &a]);
    process.expect_body("alpha body", TIMEOUT);
    process.expect_status("Ready");
    process.send("界\x1b[19~".as_bytes()); // Next tab (F8).
    process.expect_body("beta body", TIMEOUT);
    process.send(b"B\x1b[18~"); // Previous tab (F7).
    process.expect_body("界alpha body", TIMEOUT);
    process.send(b"\x1a\x1b[19~");
    process.expect_body("Bbeta body", TIMEOUT);
    process.send(b"\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha body");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "Bbeta body");
    process.send(b"\x17");
    process.expect_body("alpha body", TIMEOUT);
    process.send(b"\x17");
    process.expect("No open tabs");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Cancelling quit on a later tab must retain every earlier unsaved buffer.
#[test]
fn cancel_quit_preserves_tabs_after_earlier_discard_choice() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.txt");
    let second = directory.path().join("second.txt");
    std::fs::write(&first, "").unwrap();
    std::fs::write(&second, "").unwrap();
    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), [&first, &second]);
    process.expect_status("Ready");
    process.send(b"first-unsaved");
    process.expect_body("first-unsaved", TIMEOUT);
    process.send(b"\x1b[19~");
    process.expect_active_tab("second.txt");
    process.send(b"second-unsaved");
    process.expect_body("second-unsaved", TIMEOUT);
    process.send(b"\x11");
    process.expect_quit_confirmation("first.txt");
    process.send(b"\t\t\r");
    process.expect_quit_confirmation("second.txt");
    process.send(b"\r"); // Default Cancel.
    process.expect_absent("Unsaved changes");
    process.expect_body("second-unsaved", TIMEOUT);
    process.send(b"\x1b[18~");
    process.expect_body("first-unsaved", TIMEOUT);
    process.send(b"\x11");
    process.expect_quit_confirmation("first.txt");
    process.send(b"\t\t\r");
    process.expect_quit_confirmation("second.txt");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
}

/// The picker navigates native entries and accepts a pasted relative path literally.
#[test]
fn picker_browses_directories_and_opens_literal_pasted_path() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.txt");
    let child = directory.path().join("child");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(&original, "original body").unwrap();
    std::fs::write(child.join(".file with spaces"), "picked body").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&original));
    process.expect_status("Ready");
    process.send(b"\x0f");
    process.expect("Open file");
    process.expect("child/");
    process.send(b"\r");
    process.expect(".file with spaces");
    process.send(b"\t\x1b[200~.file with spaces\x1b[201~\r");
    process.expect_body("picked body", TIMEOUT);
    process.send(b"\x1b[18~");
    process.expect_body("original body", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A double click uses the same file/directory activation as keyboard Enter.
#[test]
fn picker_double_click_enters_folder_and_opens_file() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.txt");
    let folder = directory.path().join("nested");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(&original, "original").unwrap();
    std::fs::write(folder.join("picked.txt"), "selected file payload").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&original));
    process.expect_status("Ready");
    process.send(b"\x0f");
    process.expect("Open file");
    process.click_label("nested/", 2);
    process.click_label("picked.txt", 2);
    process.expect_body("selected file payload", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// No-argument launch presents existing commands without allocating a scratch.
#[test]
fn fresh_launch_waits_for_explicit_new_scratch() {
    let state = tempfile::tempdir().unwrap();
    let profile = state.path().join("state");
    let launch =
        || PtyChild::editor_with_state(Some("xterm-256color"), [] as [&str; 0], Some(&profile));
    let mut process = launch();
    process.expect_body("Open File (Ctrl+O)", TIMEOUT);
    process.expect_body("New Scratch (Ctrl+N)", TIMEOUT);
    process.expect_absent("Untitled");
    process.send(b"\x11");
    assert!(process.wait().success());
    let mut process = launch();
    process.expect_body("New Scratch (Ctrl+N)", TIMEOUT);
    process.click_label("New Scratch (Ctrl+N)", 1);
    process.expect_status("Ready");
    process.expect("Untitled 1");
    process.send(b"retained welcome scratch");
    process.expect_body("retained welcome scratch", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();

    let missing = state.path().join("missing-startup");
    let mut process =
        PtyChild::editor_with_state(Some("xterm-256color"), [&missing], Some(&profile));
    process.expect("Open missing-startup:");
    process.send(b"\r");
    process.expect_body("retained welcome scratch", TIMEOUT);
    process.expect_active_tab("~Untitled 1");
    process.expect_absent("Untitled 2");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Opt-in system copy sends an OSC 52 write as well as filling the internal clipboard.
#[test]
fn enabled_system_copy_sends_terminal_clipboard_request() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("ede");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[clipboard]\ncopy_to_system = true\n",
    )
    .unwrap();
    let file = directory.path().join("clip.txt");
    std::fs::write(&file, "界\nz").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&file],
        None,
        Some(directory.path()),
        None,
    );
    process.expect_status("Ready");
    process.send(b"\x01\x03");
    process.expect_raw("\x1b]52;c;55WMCno=\x1b\\");
    process.expect_status("Terminal copy sent | unconfirmed");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A temporarily paused terminal reader must not lose the session mid-frame.
#[test]
fn maximum_clipboard_frame_survives_a_paced_terminal_reader() {
    use base64::Engine;

    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("ede");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[clipboard]\ncopy_to_system = true\n",
    )
    .unwrap();
    let file = directory.path().join("clip.txt");
    let bytes = vec![b'x'; 8 * 1024];
    std::fs::write(&file, &bytes).unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&file],
        None,
        Some(directory.path()),
        None,
    );
    process.resize(40, 8);
    process.expect_status("Ready");
    process.send(b"\x01");
    process.expect_status("Col 8193");
    process.reset_output();
    process.send(b"\x18");
    let mut prefix = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    while !prefix.ends_with(b"\x1b]52;") {
        let mut byte = [0];
        match process.master.read(&mut byte) {
            Ok(1) => prefix.push(byte[0]),
            Ok(_) => thread::sleep(Duration::from_millis(1)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("PTY read: {error}"),
        }
        assert!(Instant::now() < deadline, "clipboard frame did not begin");
    }
    thread::sleep(Duration::from_millis(150));
    process.parser.process(&prefix);
    process.output.extend_from_slice(&prefix);
    let payload = base64::engine::general_purpose::STANDARD.encode(&bytes);
    process.expect_raw(&format!("\x1b]52;c;{payload}\x1b\\"));
    process.expect_status("Terminal copy sent | unconfirmed");
    process.send(b"\x16\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(file).unwrap(), bytes);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Backpressure during OSC delivery preserves external signal status and termios.
#[test]
fn clipboard_backpressure_preserves_signal_exit_status() {
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("ede");
        std::fs::create_dir(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            "[clipboard]\ncopy_to_system = true\n",
        )
        .unwrap();
        let file = directory.path().join("clip.txt");
        std::fs::write(&file, vec![b'x'; 8192]).unwrap();
        let mut process = PtyChild::editor_with_options(
            Some("xterm-256color"),
            [&file],
            None,
            Some(directory.path()),
            None,
        );
        process.resize(40, 8);
        process.expect_status("Ready");
        let pid = process.editor_pid();
        process.send(b"\x01");
        process.expect_status("Col 8193");
        process.reset_output();
        process.send(b"\x03");
        let deadline = Instant::now() + TIMEOUT;
        let mut prefix = Vec::new();
        while !prefix.ends_with(b"\x1b]52;") {
            let mut byte = [0];
            match process.master.read(&mut byte) {
                Ok(1) => prefix.push(byte[0]),
                Ok(_) => thread::sleep(Duration::from_millis(1)),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("PTY read: {error}"),
            }
            assert!(Instant::now() < deadline, "clipboard frame did not begin");
        }
        kill(pid, signal).unwrap();
        // Keep the tty open without draining it until the editor leaves the writer.
        let deadline = Instant::now() + TIMEOUT;
        while kill(pid, None).is_ok() {
            assert!(Instant::now() < deadline, "signal did not stop the editor");
            thread::sleep(Duration::from_millis(5));
        }
        process.parser.process(&prefix);
        process.output.extend_from_slice(&prefix);
        assert_eq!(process.wait().code(), Some(128 + signal as i32));
        assert_eq!(process.restored.as_ref().unwrap(), &process.original);
    }
}

/// An oversized system request retains the complete internal clipboard.
#[test]
fn oversized_system_copy_is_internal_and_round_trips() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("ede");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[clipboard]\ncopy_to_system = true\n",
    )
    .unwrap();
    let file = directory.path().join("clip.txt");
    let text = "x".repeat(8193);
    std::fs::write(&file, &text).unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&file],
        None,
        Some(directory.path()),
        None,
    );
    process.expect_status("Ready");
    process.send(b"\x01\x03");
    process.expect_status("system limit 8 KiB");
    assert!(!process.output.windows(5).any(|bytes| bytes == b"\x1b]52;"));
    process.send(b"\x1b[1;5F\x16\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read_to_string(file).unwrap(), text.repeat(2));
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Legacy System opts into automatic routing and keeps oversized terminal copies.
#[test]
fn oversized_legacy_system_copy_is_retained() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("ede");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[clipboard]\nmode = \"system\"\n",
    )
    .unwrap();
    let file = directory.path().join("clip.txt");
    let text = "x".repeat(8193);
    std::fs::write(&file, &text).unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&file],
        None,
        Some(directory.path()),
        None,
    );
    process.expect_status("Ready");
    process.send(b"\x01\x03");
    process.expect_status("Copied internally | system limit 8 KiB");
    assert!(!process.output.windows(5).any(|bytes| bytes == b"\x1b]52;"));
    process.send(b"\x1b[1;5F\x16\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text.repeat(2));
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Active stdin can copy by way of the controlling terminal, while Cut stays read-only.
#[test]
fn system_clipboard_uses_the_separate_tty_during_a_pipe() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("ede");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[clipboard]\ncopy_to_system = true\n",
    )
    .unwrap();
    let (mut process, mut producer) = piped_editor_with_options(&[], Some(directory.path()));
    producer.write_all(b"abc\n").unwrap();
    process.expect_body("abc", TIMEOUT);
    process.expect_status("User config");
    process.send(b"\x01\x03");
    process.expect_raw("\x1b]52;c;YWJjCg==\x1b\\");
    process.reset_output();
    process.send(b"\x18");
    process.expect_status("Document is read-only");
    assert!(!process.output.windows(5).any(|bytes| bytes == b"\x1b]52;"));
    drop(producer);
    process.expect_status("Editable scratch");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Legacy opt-out survives an unrelated save and can be removed by Restore Defaults.
#[test]
fn clipboard_opt_out_survives_save_and_restore_defaults_removes_it() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("ede");
    std::fs::create_dir(&config).unwrap();
    let config_file = config.join("config.toml");
    std::fs::write(
        &config_file,
        "[clipboard]\ncopy_to_system = false # private\n",
    )
    .unwrap();
    let source = directory.path().join("source.txt");
    let other = directory.path().join("other.txt");
    std::fs::write(&source, "abc").unwrap();
    std::fs::write(&other, "outside").unwrap();
    let launch = |safe| {
        let mut args = vec![source.as_os_str(), other.as_os_str()];
        if safe {
            args.insert(0, std::ffi::OsStr::new("--safe-config"));
        }
        PtyChild::editor_with_options(
            Some("xterm-256color"),
            args,
            None,
            Some(directory.path()),
            None,
        )
    };
    let mut process = launch(false);
    process.expect_status("Ready");
    process.send(b"\x1b[15~");
    process.expect("Checkpoint interval");
    process.send(b"\r\x7f2\t\r");
    process.expect_status("Settings saved");
    assert!(
        std::fs::read_to_string(&config_file)
            .unwrap()
            .contains("copy_to_system = false # private")
    );
    process.send(b"\x1b");
    process.expect_absent("Checkpoint interval");

    let check_private = |process: &mut PtyChild| {
        process.reset_output();
        process.send(b"\x01\x18");
        process.expect_status("Cut internally");
        process.send(b"\x1a\x1b[19~");
        process.expect_body("outside", TIMEOUT);
        process.send(b"\x01\x16");
        process.expect_body("abc", TIMEOUT);
        process.send(b"\x1a");
        process.expect_body("outside", TIMEOUT);
        assert!(!process.output.windows(5).any(|bytes| bytes == b"\x1b]52;"));
        process.send(b"\x1b[18~");
        process.expect_body("abc", TIMEOUT);
    };
    check_private(&mut process);

    process.send(b"\x1b[15~");
    process.click_label("Restore Defaults", 1);
    process.click_label("[Save]", 1);
    process.expect_status("Settings saved");
    assert!(
        !std::fs::read_to_string(&config_file)
            .unwrap()
            .contains("copy_to_system")
    );
    process.send(b"\x1b");
    process.expect_absent("Checkpoint interval");
    process.send(b"\x01\x03");
    process.expect_raw("\x1b]52;c;YWJj\x1b\\");
    process.send(b"\x11");
    assert!(process.wait().success());

    let mut safe = launch(true);
    safe.expect_status("Ready");
    check_private(&mut safe);
    safe.send(b"\x11");
    assert!(safe.wait().success());
}

/// Earlier accepted input is applied before a close prompt, even at a fresh tab.
#[test]
fn close_unnamed_save_as_cancel_collision_and_success() {
    let directory = tempfile::tempdir().unwrap();
    let other = directory.path().join("existing.txt");
    let saved = directory.path().join("saved.txt");
    std::fs::write(&other, "untouched").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&other));
    process.expect_status("Ready");
    process.send(b"\x0e");
    process.expect("Untitled 1");
    process.send(b"local edits\x17");
    process.expect("Close — Untitled 1");
    process.send(b"\t\r");
    process.expect("Save As (new path)");
    process.send(b"\x1b");
    process.expect_body("local edits", TIMEOUT);
    process.expect_absent("Save As (new path)");
    process.send(b"\x17");
    process.expect("Close — Untitled 1");
    process.send(b"\t\r");
    process.expect("Save As (new path)");
    process.send(other.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect("destination is busy");
    process.send(b"\x1b");
    process.expect_body("local edits", TIMEOUT);
    process.expect_absent("Save As (new path)");
    process.expect_absent("Details");
    process.send(b"\x17");
    process.expect("Close — Untitled 1");
    process.send(b"\t\r");
    process.expect("Save As (new path)");
    process.send(saved.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_body("untouched", TIMEOUT);
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), "local edits");
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "untouched");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Report every startup error, retain successful neighbors, and fall back if all fail.
#[test]
fn startup_reports_each_failure_and_falls_back() {
    let directory = tempfile::tempdir().unwrap();
    let a = directory.path().join("missing-A");
    let b = directory.path().join("missing-B");
    let good = directory.path().join("-dash");
    std::fs::write(&good, "dash body").unwrap();
    let mut process = PtyChild::editor_with_args(
        Some("xterm-256color"),
        [
            std::ffi::OsStr::new("--"),
            a.as_os_str(),
            good.as_os_str(),
            b.as_os_str(),
        ],
    );
    process.expect("Open missing-A:");
    process.send(b"\r");
    process.expect("Open missing-B:");
    process.expect("Details");
    process.send(b"\r");
    process.expect_absent("Details");
    process.expect_body("dash body", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();

    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), [&a, &b]);
    process.expect("Open missing-A:");
    process.send(b"\r");
    process.expect("Open missing-B:");
    process.expect("Details");
    process.send(b"\r");
    process.expect_absent("Details");
    process.expect_body("New Scratch (Ctrl+N)", TIMEOUT);
    process.expect_absent("Untitled 1");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// The overflow list closes an inactive tab by mouse and rejects a stale strip release.
#[test]
fn mouse_tabs_overflow_and_stale_release() {
    let directory = tempfile::tempdir().unwrap();
    let names: Vec<_> = (0..7)
        .map(|n| directory.path().join(format!("tab-{n}")))
        .collect();
    for (n, path) in names.iter().enumerate() {
        std::fs::write(path, format!("body-{n}")).unwrap();
    }
    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), &names);
    process.expect_body("body-0", TIMEOUT);
    process.expect_status("Ready");
    process.send(b"\x1b[<0;14;1M\x1b[19~"); // Press the next tab, then switch via F8.
    process.expect_body("body-1", TIMEOUT);
    process.send(b"\x1b[<0;14;1m");
    process.click_label("[Tabs]", 1);
    process.expect("Tabs — Enter to switch");
    process.expect("tab-6");
    // Pending startup loads can still change the layout between a press and release.
    process.expect_absent("…");
    process.expect_absent("↓");
    process.click_label("tab-5", 1);
    process.click_label("[Switch]", 1);
    process.expect_body("body-5", TIMEOUT);
    process.send(b"\x1b[18~\x1b[18~\x1b[18~\x1b[18~");
    process.expect_body("body-1", TIMEOUT);
    process.click_label("[Tabs]", 1);
    process.expect("Tabs — Enter to switch");
    process.click_label("tab-6", 1);
    syntax_color(&mut process, 9, 7, vt100::Color::Idx(6));
    process.resize(40, 8);
    process.expect("Tabs — Enter to switch");
    process.expect("[Back]");
    process.expect("[Details]");
    syntax_color(&mut process, 4, 3, vt100::Color::Idx(1));
    syntax_color(&mut process, 5, 10, vt100::Color::Idx(2));
    process.click_label("[Close Selected Tab]", 1);
    process.expect_absent("Tabs — Enter to switch");
    process.resize(40, 8);
    process.expect("Tabs");
    process.expect_status("Ready");
    process.send(b"\x1b[18~\x1b[18~");
    process.expect_body("body-5", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A real interactive shell owns job control: suspend returns cooked mode and fg resumes safely.
#[test]
fn suspend_and_resume_under_job_control() {
    let config = tempfile::tempdir().unwrap();
    let root = config.path().join("ede");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("config.toml"),
        "[keybindings.global]\nquit = ['Ctrl+B']",
    )
    .unwrap();
    let mut shell = Command::new("/bin/sh");
    shell.arg("-i").env("XDG_CONFIG_HOME", config.path());
    let mut process = PtyChild::spawn(shell);
    process.expect_raw("EDE_TEST_PROMPT>");
    // Shells can select their own cooked settings; capture exactly what the application inherits.
    process.original = termios::tcgetattr(&process.slave).unwrap();
    process.reset_output();
    process.send(
        format!(
            "/bin/sh -c 'printf \"EDE_PID:%s\\n\" \"$$\"; exec \"$1\"' ede-child '{}'\n",
            env!("CARGO_BIN_EXE_ede")
        )
        .as_bytes(),
    );
    process.expect_status("User config");
    process.expect_body("New Scratch", TIMEOUT);
    let foreground = process.editor_pid();
    process.reset_output();
    process.send(b"\x1bOP");
    process.expect("Cmd aliases require");
    process.send(b"\t\r");
    process.expect_raw("EDE_TEST_PROMPT>");
    process.assert_restored();
    process.resize(60, 16);
    process.reset_output();
    process.send(b"fg\n");
    process.expect("Cmd aliases require");
    assert!(kill(foreground, None).is_ok());
    process.reset_output();
    // External SIGTSTP follows the same cleanup route as the help action.
    kill(foreground, Signal::SIGTSTP).unwrap();
    process.expect_raw("EDE_TEST_PROMPT>");
    process.assert_restored();
    process.reset_output();
    process.send(b"fg\n");
    process.expect("Cmd aliases require");
    process.reset_output();
    process.send(b"\x1b");
    process.expect_absent("Cmd aliases require");
    process.send(b"\x02");
    process.expect_raw("EDE_TEST_PROMPT>");
    process.assert_restored();
    process.send(b"exit\n");
    assert!(process.wait().success());
}

/// File edits, Unicode, guarded quit, undo and acknowledged save points work through actual input.
#[test]
fn open_edit_save_and_dirty_quit_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file with spaces.txt");
    std::fs::write(&path, b"start\r\nsecond").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_status("Ready");
    process.expect_body("start", TIMEOUT);
    process.send("\x1b[F界e\u{301}".as_bytes());
    process.expect("start界e\u{301}");
    process.send(b"\x13");
    process.expect_status("Saved");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        "start界e\u{301}\r\nsecond".as_bytes()
    );
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();

    // Reopening exercises the metadata left by the first save as a source baseline.
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_status("Ready");
    process.expect_body("start界e\u{301}", TIMEOUT);
    process.send(b"+");
    process.expect_body("+start界e\u{301}", TIMEOUT);
    process.send(b"\x13");
    process.expect_status("Saved");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        "+start界e\u{301}\r\nsecond".as_bytes()
    );
    process.send(b"!");
    process.expect_body("+!start界e\u{301}", TIMEOUT);
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\r");
    process.expect_absent("Unsaved changes");
    process.send(b"\x1a");
    process.expect_body("+start界e\u{301}", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        "+start界e\u{301}\r\nsecond".as_bytes()
    );
}

/// Terminal paste normalizes every received line ending to the destination style.
#[test]
fn terminal_paste_restores_line_breaks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pasted.txt");
    for (original, expected) in [
        (&b""[..], &b"one\ntwo\n\nthree\nfour\nfive\n!"[..]),
        (
            &b"original\r\nother\n"[..],
            &b"one\r\ntwo\r\n\r\nthree\r\nfour\r\nfive\r\n!original\r\nother\n"[..],
        ),
    ] {
        std::fs::write(&path, original).unwrap();
        let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
        process.expect_status("Ready");
        process.send(b"\x1b[200~one\rtwo\r\rthree\r\nfour\nfive\r\x1b[201~");
        process.expect_body("five", TIMEOUT);
        process.send(b"!\x13");
        process.expect_status("Saved");
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        process.send(b"\x11");
        assert!(process.wait().success());
        process.assert_restored();
    }
}

/// Settings changes apply immediately only when the open document lacks a line ending.
#[test]
fn default_line_ending_applies_to_open_documents_without_endings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("paste.txt");
    let config = directory.path().join("ede/config.toml");
    std::fs::write(&path, "seed").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&path],
        None,
        Some(directory.path()),
        None,
    );
    process.resize(40, 8);
    process.expect_status("Ready");
    process.send(b"\x1b[15~");
    process.expect("[Editor]");
    process.send(b"\x1b[Z\x1b[C\x1b[C");
    process.expect("Default line ending · LF");
    process.send(b"\r\r");
    process.expect("Default line ending · CRLF");
    assert!(!config.exists());
    process.click_label("[Discard]", 1);
    process.expect("Discard unsaved settings?");
    process.click_label("[Discard]", 1);
    process.expect("Default line ending · LF");
    process.send(b"\r");
    process.expect("Default line ending · CRLF");
    process.click_label("[Save]", 1);
    process.expect_status("Settings saved");
    assert!(
        std::fs::read_to_string(&config)
            .unwrap()
            .contains("[editor]\ndefault_line_ending = \"crlf\"")
    );
    process.resize(64, 8);
    process.click_label("[Keyboard Shortcuts]", 1);
    process.expect(">Keyboard Shortcuts<");
    process.expect("App commands");
    process.send(b"\r\x1b[C");
    process.expect("Editing text");
    process.expect("[Keyboard Shortcuts]");
    process.send(b"\x1b");
    process.expect_absent("Keyboard Shortcuts");
    process.resize(80, 24);
    run_action(&mut process, "Tabs", 2);
    process.expect("Line ending: CRLF");
    process.send(b"\x1b");
    process.expect_absent("Tab Details");
    process.send(b"\r\x1b[200~first\nsecond\x1b[201~");
    process.expect_body("secondseed", TIMEOUT);
    process.send(b"\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(&path).unwrap(), b"\r\nfirst\r\nsecondseed");

    process.resize(80, 24);
    process.send(b"\x1b[15~");
    process.click_label("[Editor]", 1);
    process.click_label("Default line ending · CRLF", 1);
    process.expect("Default line ending · LF");
    process.click_label("[Save]", 1);
    process.expect_status("Settings saved");
    assert!(
        std::fs::read_to_string(&config)
            .unwrap()
            .contains("default_line_ending = \"lf\"")
    );
    process.send(b"\x1b");
    process.expect_absent("Default line ending");
    run_action(&mut process, "Tabs", 2);
    process.expect("Line ending: CRLF");
    process.send(b"\x1b");
    process.expect_absent("Tab Details");
    process.send(b"\r\x1b[200~third\nfourth\x1b[201~");
    process.expect_body("fourthseed", TIMEOUT);
    process.send(b"\x13");
    process.expect_status("Saved");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"\r\nfirst\r\nsecond\r\nthird\r\nfourthseed"
    );

    process.send(b"\x01\x7f");
    run_action(&mut process, "Tabs", 2);
    process.expect("Line ending: LF");
    process.send(b"\x1b");
    process.expect_absent("Tab Details");
    process.send(b"\x1b[200~clean\r\nend\x1b[201~");
    process.expect_body("clean", TIMEOUT);
    process.send(b"\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(&path).unwrap(), b"clean\nend");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// The terminal parser and document path accept a multi-megabyte paste with one undo unit.
#[test]
fn external_large_paste_save_undo_and_redo() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pasted.txt");
    let mut process = PtyChild::editor();
    process.expect("Ready");
    let mut payload = "line界\r\n".repeat(262_144);
    payload.push_str("END-PAYLOAD");
    assert!(payload.len() > 1 << 20);
    let started = Instant::now();
    process.send(b"\x1b[200~");
    process.send(payload.as_bytes());
    process.send(b"\x1b[201~");
    process.expect("END-PAYLOAD");
    process.send(b"\x13");
    process.expect("Save As (new path)");
    process.send(path.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_status("Saved");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        payload.replace("\r\n", "\n").as_bytes()
    );
    eprintln!(
        "external paste: {} bytes, receive/render/save {:?}",
        payload.len(),
        started.elapsed()
    );
    process.send(b"\x1a\x11");
    process.expect("Unsaved changes");
    process.send(b"\r");
    thread::sleep(Duration::from_millis(30));
    process.send(b"\x19\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A dirty Quit prompt requested at tiny geometry is usable once visible.
#[test]
fn dirty_quit_at_tiny_size_requires_visible_confirmation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("guarded.txt");
    std::fs::write(&path, "").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("Ready");
    process.send(b"work");
    process.expect_body("work", TIMEOUT);
    process.resize(20, 4);
    process.expect("Resize");
    process.send(b"\x11");
    process.expect("Escape Cancel");
    process.resize(80, 24);
    process.expect("Unsaved changes");
    process.send(b"\x1b");
    process.expect_absent("Unsaved changes");
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Synthetic protocol delivery exercises Cmd/Super and exact Shift, then restores the pushed mode.
#[test]
fn enhanced_cmd_delivery_and_restoration() {
    let mut process = PtyChild::editor();
    process.expect_raw("\x1b[?u");
    process.send(b"\x1b[?1u");
    process.expect_raw("\x1b[>1u");
    process.expect("Ready");
    process.send(b"\x1b[115;10u");
    process.expect("Save As (new path)");
    process.send(b"\x1b");
    thread::sleep(Duration::from_millis(60));
    process.send(b"\x1b[113;9u");
    assert!(process.wait().success());
    process.assert_restored();
    assert!(process.output.windows(5).any(|bytes| bytes == b"\x1b[<1u"));
}

/// I02: checkpoint/restore a large scratch while a GiB refresh keeps another tab usable.
#[test]
#[ignore = "large recovery/refresh benchmark: EDE_BENCH_MIB=1024 EDE_PASTE_MIB=64"]
fn large_recovery_and_refresh_benchmark() {
    use std::os::unix::fs::{FileExt, MetadataExt};
    let mib: u64 = std::env::var("EDE_BENCH_MIB")
        .unwrap_or_else(|_| "1024".into())
        .parse()
        .unwrap();
    let scratch_mib: u64 = std::env::var("EDE_PASTE_MIB")
        .unwrap_or_else(|_| "64".into())
        .parse()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("state");
    let path = directory.path().join("large");
    let replacement = directory.path().join("replacement");
    let companion = directory.path().join("companion");
    let mut chunk = vec![b'x'; 1 << 20];
    for index in (63..chunk.len()).step_by(64) {
        chunk[index] = b'\n';
    }
    for (fixture, marker) in [
        (&path, b"ORIGINAL".as_slice()),
        (&replacement, b"REFRESHED".as_slice()),
    ] {
        let mut file = File::create(fixture).unwrap();
        for index in 0..mib {
            file.write_all(&chunk).unwrap();
            if (index + 1).is_multiple_of(8) {
                file.sync_data().unwrap();
            }
        }
        file.write_all_at(marker, 0).unwrap();
        file.sync_all().unwrap();
    }
    eprintln!(
        "recovery_refresh fixtures_ready named_bytes={} replacement_bytes={} scratch_MiB={scratch_mib}",
        mib * (1 << 20),
        mib * (1 << 20)
    );
    std::fs::write(&companion, "Companion").unwrap();
    let started = Instant::now();
    let mut process =
        PtyChild::editor_with_state(Some("xterm-256color"), [&path, &companion], Some(&profile));
    process.expect_body("ORIGINAL", Duration::from_secs(120));
    let first_text = started.elapsed();
    process.expect_with_timeout("Ready", Duration::from_secs(120));
    let ready = started.elapsed();
    let pid = process.editor_pid();
    let rss = || {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.as_raw().to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let mut peak_rss = rss();
    process.send(b"\x0e");
    process.expect("Untitled");
    process.expect_status("Ready");
    let paste_started = Instant::now();
    process.send(b"\x1b[200~");
    for index in 0..scratch_mib * 16 {
        process.send(&chunk[..64 << 10]);
        if index.is_multiple_of(128) {
            peak_rss = peak_rss.max(rss());
        }
    }
    process.send(b"SCRATCH_END\x1b[201~");
    process.expect_body("SCRATCH_END", Duration::from_secs(120));
    let paste_ready = paste_started.elapsed();
    process.send(b"\x1b[18~");
    process.expect_body("Companion", TIMEOUT);
    std::fs::rename(&replacement, &path).unwrap();
    let refresh_started = Instant::now();
    let mut latency = Vec::new();
    for count in 1..=50 {
        let event = Instant::now();
        process.send(b"Z");
        process.expect_body(&"Z".repeat(count), TIMEOUT);
        latency.push(event.elapsed().as_secs_f64() * 1000.0);
        peak_rss = peak_rss.max(rss());
        thread::sleep(Duration::from_millis(100));
    }
    process.send(b"\x1b[18~");
    process.expect_body("REFRESHED", Duration::from_secs(120));
    let refresh_visible = refresh_started.elapsed();
    latency.sort_by(f64::total_cmp);
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    let quit = Instant::now();
    assert!(process.wait().success());
    let flush = quit.elapsed();
    process.assert_restored();
    let mut object_bytes = 0;
    let mut object_allocated = 0;
    for entry in std::fs::read_dir(profile.join("objects")).unwrap() {
        let metadata = entry.unwrap().metadata().unwrap();
        object_bytes += metadata.len();
        object_allocated += metadata.blocks() * 512;
    }
    let restored_at = Instant::now();
    let mut restored = PtyChild::editor_with_state(
        Some("xterm-256color"),
        std::iter::empty::<&str>(),
        Some(&profile),
    );
    restored.expect_body("SCRATCH_END", Duration::from_secs(120));
    let restore = restored_at.elapsed();
    restored.send(b"\x11");
    assert!(restored.wait().success());
    restored.assert_restored();
    eprintln!(
        "recovery_refresh named_MiB={mib} scratch_MiB={scratch_mib} first_text_ms={:.3} ready_ms={:.3} paste_ready_ms={:.3} refresh_observed_ms={:.3} quit_flush_ms={:.3} restore_ms={:.3} input_p50_ms={:.3} input_p95_ms={:.3} input_max_ms={:.3} sampled_RSS_KiB={peak_rss} recovery_logical_bytes={object_bytes} recovery_allocated_bytes={object_allocated}",
        first_text.as_secs_f64() * 1000.0,
        ready.as_secs_f64() * 1000.0,
        paste_ready.as_secs_f64() * 1000.0,
        refresh_visible.as_secs_f64() * 1000.0,
        flush.as_secs_f64() * 1000.0,
        restore.as_secs_f64() * 1000.0,
        latency[25],
        latency[47],
        latency[49]
    );
    process.report_peak_rss();
}

/// Real allocated-file terminal benchmark; opt in so ordinary tests remain small and repeatable.
#[test]
#[ignore = "large-file benchmark: run explicitly with EDE_BENCH_MIB and EDE_BENCH_CORPUS"]
fn large_file_interactive_benchmark() {
    let mib: u64 = std::env::var("EDE_BENCH_MIB")
        .unwrap_or_else(|_| "1024".into())
        .parse()
        .unwrap();
    let corpus = std::env::var("EDE_BENCH_CORPUS").unwrap_or_else(|_| "ordinary".into());
    let pattern: &[u8] = match corpus.as_str() {
        "ordinary" => b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef012\n",
        "dense" => b"x\n",
        "long" => b"x",
        _ => panic!("invalid corpus"),
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("allocated.txt");
    let mut file = File::create(&path).unwrap();
    let mut chunk = vec![0; 1 << 20];
    for (index, byte) in chunk.iter_mut().enumerate() {
        *byte = pattern[index % pattern.len()];
    }
    let length = mib.checked_mul(1 << 20).unwrap();
    let mut remaining = length;
    let mut lines = 1u64;
    while remaining > 0 {
        let count = remaining.min(chunk.len() as u64) as usize;
        file.write_all(&chunk[..count]).unwrap();
        lines += chunk[..count].iter().filter(|byte| **byte == b'\n').count() as u64;
        remaining -= count as u64;
        if remaining.is_multiple_of(8 << 20) {
            file.sync_data().unwrap();
        }
    }
    file.sync_all().unwrap();
    drop(file);
    eprintln!(
        "fixture={corpus} bytes={length} logical_lines={lines} generated warm on {}",
        directory.path().display()
    );
    let other = directory.path().join("companion.txt");
    std::fs::write(&other, "Companion tab").unwrap();
    let started = Instant::now();
    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), [&path, &other]);
    let first = if corpus == "ordinary" {
        "0123456789abcdef"
    } else {
        "x"
    };
    process.expect_body(first, Duration::from_secs(120));
    let first_text = started.elapsed();
    process.expect_with_timeout("Ready", Duration::from_secs(120));
    let ready = started.elapsed();
    process.send(b"\x1b[19~");
    process.expect_active_tab("companion.txt");
    process.expect_body("Companion tab", TIMEOUT);
    process.send(b"\x1b[18~");
    process.expect_active_tab("allocated.txt");
    process.expect_body(first, TIMEOUT);
    let pid = process.editor_pid();
    let rss = || {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.as_raw().to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let mut sampled_rss = rss();
    let mut latency = Vec::new();
    for count in 1..=40 {
        let event = Instant::now();
        process.send(b"Z");
        process.expect_body(&"Z".repeat(count), TIMEOUT);
        latency.push(event.elapsed().as_secs_f64() * 1000.0);
        sampled_rss = sampled_rss.max(rss());
    }
    process.send(b"\x07");
    process.expect("Go to line[:cell]");
    let middle = if corpus == "long" {
        // The first 40 edits already shifted the original midpoint by 40 cells.
        format!("1:{}", length / 2 + 41)
    } else {
        format!("{}", lines / 2 + 1)
    };
    process.send(middle.as_bytes());
    process.send(b"\rB");
    process.expect_body("B", Duration::from_secs(120));
    sampled_rss = sampled_rss.max(rss());
    process.send(b"\x1b[1;5FC");
    process.expect_body("C", Duration::from_secs(120));
    process.send(b"\x13");
    // Continue navigation and typing while the immutable save is in flight.
    process.send(b"\x1b[1;5HD");
    process.expect_body("D", Duration::from_secs(120));
    process.expect_with_timeout("Saved", Duration::from_secs(120));
    sampled_rss = sampled_rss.max(rss());
    latency.sort_by(f64::total_cmp);
    eprintln!(
        "UI corpus={corpus} MiB={mib} first_text_ms={:.3} ready_ms={:.3} ordinary_samples={} p50_ms={:.3} p95_ms={:.3} max_ms={:.3} sampled_max_editor_RSS_KiB={sampled_rss}",
        first_text.as_secs_f64() * 1000.0,
        ready.as_secs_f64() * 1000.0,
        latency.len(),
        latency[latency.len() / 2],
        latency[latency.len() * 95 / 100],
        latency.last().unwrap()
    );
    // The later D was not in the captured save, so normal quit must still protect it.
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
    process.report_peak_rss();
    let file = File::open(&path).unwrap();
    assert_eq!(file.metadata().unwrap().len(), length + 42);
    let mut reader = io::BufReader::new(file);
    let mut actual = vec![0; chunk.len()];
    check_saved_chunk(&mut reader, &mut actual, &[b'Z'; 40]);
    let mut remaining = length;
    let mut newlines = lines / 2;
    let mut inserted = false;
    while remaining > 0 {
        let count = remaining.min(chunk.len() as u64) as usize;
        let expected = &chunk[..count];
        let middle = if inserted {
            None
        } else if corpus == "long" {
            let start = length - remaining;
            (start..start + count as u64)
                .contains(&(length / 2))
                .then(|| (length / 2 - start) as usize)
        } else if newlines == 0 {
            Some(0)
        } else {
            memchr::memchr_iter(b'\n', expected).find_map(|index| {
                newlines -= 1;
                (newlines == 0).then_some(index + 1)
            })
        };
        if let Some(middle) = middle {
            check_saved_chunk(&mut reader, &mut actual, &expected[..middle]);
            check_saved_chunk(&mut reader, &mut actual, b"B");
            check_saved_chunk(&mut reader, &mut actual, &expected[middle..]);
            inserted = true;
        } else {
            check_saved_chunk(&mut reader, &mut actual, expected);
        }
        remaining -= count as u64;
    }
    assert!(inserted, "expected a middle edit");
    check_saved_chunk(&mut reader, &mut actual, b"C");
    assert_eq!(reader.read(&mut actual).unwrap(), 0);
}

/// Compare all saved bytes in chunks without loading a file-sized expected value.
fn check_saved_chunk(reader: &mut impl Read, actual: &mut [u8], expected: &[u8]) {
    let actual = &mut actual[..expected.len()];
    reader.read_exact(actual).unwrap();
    if actual != expected {
        let first = actual
            .iter()
            .zip(expected)
            .position(|(actual, expected)| actual != expected)
            .unwrap();
        panic!(
            "saved chunk differs at byte {first} ({} vs {})",
            actual[first], expected[first]
        );
    }
}

/// Search/replace over a real larger-than-cache fixture while measuring live input.
#[test]
#[ignore = "search resource benchmark: EDE_BENCH_MIB=1024 EDE_BENCH_CORPUS=ordinary|dense|long"]
fn large_search_and_replace_benchmark() {
    let mib: u64 = std::env::var("EDE_BENCH_MIB")
        .unwrap_or_else(|_| "1024".into())
        .parse()
        .unwrap();
    let corpus = std::env::var("EDE_BENCH_CORPUS").unwrap_or_else(|_| "ordinary".into());
    assert!(mib > 0);
    let marker_stride = std::env::var("EDE_BENCH_MARKER_STRIDE_MIB")
        .map_or(1, |value| value.parse::<u64>().unwrap());
    assert!(marker_stride > 0);
    let matches = mib.div_ceil(marker_stride);
    let has_exact_count = |screen: &str, label: &str| {
        let count = format!("{matches} {label}");
        screen
            .match_indices(&count)
            .any(|(index, _)| index == 0 || !screen.as_bytes()[index - 1].is_ascii_digit())
    };
    let pattern: &[u8] = match corpus.as_str() {
        "ordinary" => b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef012\n",
        "dense" => b"x\n",
        "long" => b"x",
        _ => panic!("unknown corpus"),
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("search-benchmark");
    let mut chunk = vec![0; 1 << 20];
    for (index, byte) in chunk.iter_mut().enumerate() {
        *byte = pattern[index % pattern.len()];
    }
    let mut marked = chunk.clone();
    marked[..10].copy_from_slice(b"SEEK_TOKEN");
    let mut file = File::create(&path).unwrap();
    for index in 0..mib {
        let block = if index.is_multiple_of(marker_stride) {
            &marked
        } else {
            &chunk
        };
        file.write_all(block).unwrap();
        if (index + 1).is_multiple_of(8) {
            file.sync_data().unwrap();
        }
    }
    file.sync_all().unwrap();
    drop(file);
    eprintln!(
        "search fixture_ready corpus={corpus} bytes={} marker_stride_MiB={marker_stride} warm",
        mib << 20
    );
    let started = Instant::now();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_body("SEEK_TOKEN", Duration::from_secs(120));
    let first_text = started.elapsed();
    process.expect_with_timeout("Ready", Duration::from_secs(120));
    let ready = started.elapsed();
    let pid = process.editor_pid();
    let rss = || {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.as_raw().to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let mut peak_rss = rss();
    process.send(b"\x06SEEK_TOKEN\r");
    let scan_started = Instant::now();
    process.expect_with_timeout("exact match differs", Duration::from_secs(120));
    let screen = process.parser.screen().contents();
    assert!(has_exact_count(&screen, "matches"), "{screen}");
    let scan = scan_started.elapsed();
    process.send(b"\x06\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f");
    process.send(if corpus == "ordinary" { b"0\r" } else { b"x\r" });
    process.send(b"\x1b[17~"); // F6 returns to the live document.
    let mut latency = Vec::new();
    for count in 1..=40 {
        let event = Instant::now();
        process.send(b"Z");
        process.expect_body(&"Z".repeat(count), TIMEOUT);
        latency.push(event.elapsed().as_secs_f64() * 1000.0);
        peak_rss = peak_rss.max(rss());
    }
    let cancel_started = Instant::now();
    process.send(b"\x1b");
    let cancel_deadline = Instant::now() + Duration::from_secs(30);
    let cancellation_observed = loop {
        process.drain();
        let screen = process.parser.screen().contents();
        if screen.contains("Cancelled") {
            break true;
        }
        if !screen.contains("Find:") {
            break false;
        }
        assert!(
            Instant::now() < cancel_deadline,
            "cancel did not settle: {screen}"
        );
        thread::sleep(Duration::from_millis(1));
    };
    let cancellation = cancel_started.elapsed();
    process.send(b"\x08\x7fSEEK_TOKEN\tFOUND_DONE");
    process.click_label("[Replace All]", 1);
    process.expect("Replace all — confirm scope");
    let replace_started = Instant::now();
    process.send(b"\t\r");
    process.expect_with_timeout(
        &format!("{matches} occurrences processed"),
        Duration::from_secs(180),
    );
    let screen = process.parser.screen().contents();
    assert!(
        has_exact_count(&screen, "occurrences processed"),
        "{screen}"
    );
    let replacement = replace_started.elapsed();
    peak_rss = peak_rss.max(rss());
    process.send(b"\x1b");
    process.expect_absent("Options");
    let save_started = Instant::now();
    process.send(b"\x13");
    // Guarded Save rereads both the destination and the private captured revision.
    // Allow proportionally more time to verify a published larger-than-RAM file.
    let save_timeout = Duration::from_secs(120.max(mib.div_ceil(1024) * 60));
    process.expect_with_timeout("Saved", save_timeout);
    let saved = save_started.elapsed();
    process.send(b"\x1a");
    process.expect_body("SEEK_TOKEN", TIMEOUT);
    process.send(b"\x11");
    process.expect("Unsaved changes");
    process.send(b"\t\t\r");
    assert!(process.wait().success());
    process.assert_restored();
    process.report_peak_rss();
    let file = File::open(&path).unwrap();
    assert_eq!(file.metadata().unwrap().len(), (mib << 20) + 40);
    let mut reader = io::BufReader::new(file);
    let mut actual = vec![0; chunk.len()];
    check_saved_chunk(&mut reader, &mut actual, &[b'Z'; 40]);
    marked[..10].copy_from_slice(b"FOUND_DONE");
    for index in 0..mib {
        let block = if index.is_multiple_of(marker_stride) {
            &marked
        } else {
            &chunk
        };
        check_saved_chunk(&mut reader, &mut actual, block);
    }
    assert_eq!(reader.read(&mut actual).unwrap(), 0);
    latency.sort_by(f64::total_cmp);
    eprintln!(
        "search_ui corpus={corpus} MiB={mib} first_text_ms={:.3} ready_ms={:.3} full_scan_ms={:.3} replace_ms={:.3} save_ms={:.3} cancellation_observed={cancellation_observed} cancellation_ms={:.3} input_p50_ms={:.3} input_p95_ms={:.3} input_max_ms={:.3} sampled_peak_RSS_KiB={peak_rss}",
        first_text.as_secs_f64() * 1000.0,
        ready.as_secs_f64() * 1000.0,
        scan.as_secs_f64() * 1000.0,
        replacement.as_secs_f64() * 1000.0,
        saved.as_secs_f64() * 1000.0,
        cancellation.as_secs_f64() * 1000.0,
        latency[20],
        latency[38],
        latency[39]
    );
}

/// Pasted path data and immediately following typing/Enter stay in order off the UI thread.
#[test]
fn pasted_save_as_path_and_immediate_submit() {
    let directory = tempfile::tempdir().unwrap();
    let stem = directory.path().join("pasted destination");
    let destination = directory.path().join("pasted destination.txt");
    let mut process = PtyChild::editor();
    process.expect("Ready");
    process.send("emoji 👩\n".as_bytes());
    process.send(b"\x13");
    process.expect("Save As (new path)");
    process.send(b"\x1b[200~");
    process.send(stem.as_os_str().as_encoded_bytes());
    process.send(b"\x1b[201~.txt\r");
    process.expect_status("Saved");
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        "emoji 👩\n".as_bytes()
    );
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// A rapid multiline drag, copy and paste preserves ordering without waiting between mouse events.
#[test]
fn text_mouse_drag_copy_paste_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mouse.txt");
    let original = "abc界def\n0123456789\nlast";
    std::fs::write(&path, original).unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("Ready");
    process.send(b"\x1b[<0;3;2M\x1b[<32;4;3M\x1b[<0;5;3m\x03\x1b[1;5F\r\x16\x13");
    let expected = format!("{original}\nc界def\n0123");
    let deadline = Instant::now() + TIMEOUT;
    while std::fs::read(&path).unwrap() != expected.as_bytes() {
        process.drain();
        assert!(
            Instant::now() < deadline,
            "saved {:?}, transcript {}",
            std::fs::read_to_string(&path).unwrap(),
            String::from_utf8_lossy(&process.output)
        );
        thread::sleep(Duration::from_millis(5));
    }
    process.expect_status("Saved");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Save As refuses an existing destination and offers a new path without dropping the buffer.
#[test]
fn save_as_existing_destination_retains_both_contents() {
    let directory = tempfile::tempdir().unwrap();
    let existing = directory.path().join("existing");
    let fresh = directory.path().join("fresh");
    std::fs::write(&existing, b"external").unwrap();
    let mut process = PtyChild::editor();
    process.expect("Ready");
    process.send(b"mine\x13");
    process.expect("Save As (new path)");
    process.send(existing.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect("work retained");
    assert_eq!(std::fs::read(&existing).unwrap(), b"external");
    process.send(b"\x1bOS");
    process.expect("Save As (new path)");
    process.send(fresh.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(fresh).unwrap(), b"mine");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Separate ingress RSS from document-cache RSS while paste size grows, using fixed input chunks.
#[test]
#[ignore = "paste memory benchmark: run explicitly with EDE_PASTE_MIB"]
fn large_paste_memory_benchmark() {
    let mib: u64 = std::env::var("EDE_PASTE_MIB")
        .unwrap_or_else(|_| "64".into())
        .parse()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("paste");
    let mut process = PtyChild::editor();
    process.expect("Ready");
    let pid = process.editor_pid();
    let rss = || {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.as_raw().to_string()])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let baseline = rss();
    let mut ingress_rss = baseline;
    let chunk = vec![b'p'; 64 << 10];
    let started = Instant::now();
    process.send(b"\x1b[200~");
    for index in 0..mib * 16 {
        process.send(&chunk);
        if index.is_multiple_of(128) {
            ingress_rss = ingress_rss.max(rss());
        }
    }
    ingress_rss = ingress_rss.max(rss());
    process.send(b"PASTE_END\x1b[201~");
    process.expect_body("PASTE_END", Duration::from_secs(120));
    let ready_rss = rss();
    process.send(b"\x13");
    process.expect("Save As (new path)");
    process.send(path.as_os_str().as_encoded_bytes());
    process.send(b"\r");
    process.expect_with_timeout("Saved", Duration::from_secs(120));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), mib * (1 << 20) + 9);
    eprintln!(
        "paste_MiB={mib} baseline_RSS_KiB={baseline} sampled_ingress_max_RSS_KiB={ingress_rss} document_ready_RSS_KiB={ready_rss} receive_render_save_ms={:.3}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
    process.report_peak_rss();
}

/// Isolated tmux acceptance: portable editing/clipboard/paste, guarded saves and real job control.
#[test]
#[ignore = "requires local tmux: run explicitly with an isolated socket"]
fn isolated_tmux_editing_and_suspend_smoke() {
    use std::path::{Path, PathBuf};
    /// This socket belongs only to the test; cleanup cannot reach a user's tmux server.
    struct Server(PathBuf);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(&self.0)
                .arg("kill-server")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    /// Quote fixture paths as one shell argument without interpreting embedded shell syntax.
    fn quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }
    /// Darwin may set transient PENDIN while returning to its interactive shell.
    fn cooked(file: &File) -> Termios {
        let mut attributes = termios::tcgetattr(file).unwrap();
        attributes.local_flags.remove(termios::LocalFlags::PENDIN);
        attributes
    }
    /// Wait for the interactive shell to finish restoring its own line-editing mode.
    fn restored(pane: &File, original: &Termios) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let actual = cooked(pane);
            if actual == *original {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "shell attributes did not settle: {actual:?} != {original:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let server = Server(directory.path().join("tmux.sock"));
    let path = directory.path().join("document");
    std::fs::write(&path, b"first\nsecond").unwrap();
    let mut command = Command::new("tmux");
    command
        .arg("-S")
        .arg(&server.0)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-s",
            "ede-stage3",
            "--",
            "/bin/sh",
            "-i",
        ])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    let mut process = PtyChild::spawn(command);
    process.expect("EDE_TEST_PROMPT>");
    let tty = Command::new("tmux")
        .arg("-S")
        .arg(&server.0)
        .args(["list-panes", "-F", "#{pane_tty}"])
        .output()
        .unwrap();
    assert!(tty.status.success());
    let tty = String::from_utf8(tty.stdout).unwrap();
    let pane = File::open(tty.trim()).unwrap();
    let original = cooked(&pane);
    process.send(
        format!(
            "{} {}\n",
            quote(Path::new(env!("CARGO_BIN_EXE_ede"))),
            quote(&path)
        )
        .as_bytes(),
    );
    process.expect("Ready");
    process.send(b"X");
    process.expect("Xfirst");
    process.send(b"\x13");
    process.expect("Saved");
    assert_eq!(std::fs::read(&path).unwrap(), b"Xfirst\nsecond");
    process.send(b"\x01\x03\x1b[1;5F\r\x16I");
    process.expect("secondI");
    process.send(b"\x1b[200~-external\n\x11TAIL\x1b[201~");
    process.expect("TAIL");
    process.send(b"\x13");
    process.expect("Saved");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"Xfirst\nsecond\nXfirst\nsecondI-external\n\x11TAIL"
    );
    // Exercise the client's actual SGR mouse forwarding, including a release
    // position later than the last drag event, through the isolated tmux server.
    process.send(b"\x1b[1;5H");
    process.expect("Ln 1 Col 1");
    process.send(b"\x1b[<0;1;2M\x1b[<32;2;2M\x1b[<0;3;2m\x03\x1b[1;5F\r\x16\x13");
    process.expect("Saved");
    let expected = b"Xfirst\nsecond\nXfirst\nsecondI-external\n\x11TAIL\nXf";
    let deadline = Instant::now() + TIMEOUT;
    while std::fs::read(&path).unwrap() != expected {
        process.drain();
        assert!(
            Instant::now() < deadline,
            "{}",
            process.parser.screen().contents()
        );
        thread::sleep(Duration::from_millis(5));
    }
    // Pause native capture through the menu and keep it paused through suspend.
    run_action(&mut process, "Settings", 2);
    select_action(&mut process, "Settings", 2);
    process.expect("Mouse capture: paused");
    process.send(b"\x1b");
    let deadline = Instant::now() + TIMEOUT;
    while process
        .parser
        .screen()
        .contents()
        .contains("Mouse capture: paused")
    {
        process.drain();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    process.send(b"\x16");
    process.expect_body("XfXf", TIMEOUT);
    process.send(b"\x1a\x13");
    process.expect("Saved");
    process.send(b"\x1bOP");
    process.expect("Cmd aliases require");
    process.send(b"\t\r");
    process.expect("EDE_TEST_PROMPT>");
    restored(&pane, &original);
    // tmux can redraw the old prompt during teardown. Wait for a fresh prompt
    // after the resumed editor exits, with split quoting to exclude command echo.
    process.send(b"fg; editor_status=$?; PS1='EDE_TEST_''EXIT> '\n");
    process.expect("Cmd aliases require");
    process.send(b"\x1b");
    let deadline = Instant::now() + TIMEOUT;
    while process
        .parser
        .screen()
        .contents()
        .contains("Cmd aliases require")
    {
        process.drain();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    process.send(b"\x11");
    process.expect("EDE_TEST_EXIT>");
    restored(&pane, &original);
    process.send(b"exit \"$editor_status\"\n");
    assert!(process.wait().success());
}

/// A wheel-then-click burst must not reuse the rows above the new viewport.
#[test]
fn wheel_then_click_rejects_obsolete_cell_map() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wheel.txt");
    let content = "row0\nrow1\nrow2\nrow3\nrow4\n";
    std::fs::write(&path, content).unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("Ready");
    process.expect("row0");
    process.send(b"\x1b[B");
    process.expect_status("Ln 2 Col 1");
    // Scroll three rows down; click the top content row before a fresh screen is available.
    process.send(b"\x1b[<65;1;2M\x1b[<0;1;2M\x1b[<0;1;2m");
    process.send(b"!\x13");
    process.expect_status("Saved");
    let saved = std::fs::read_to_string(path).unwrap();
    assert!(
        saved == "row0\n!row1\nrow2\nrow3\nrow4\n" || saved == "row0\nrow1\nrow2\n!row3\nrow4\n",
        "obsolete cell map changed the row above the viewport: {saved:?}"
    );
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Replacing selected text with identical bytes still ends the selection.
#[test]
fn identical_replacement_moves_caret_without_history_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one");
    std::fs::write(&path, b"a").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_status("Ready");
    process.send(b"\x01ab\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(&path).unwrap(), b"ab");
    process.send(b"\x1a\x1a");
    process.expect_body("a", TIMEOUT);
    process.expect_status("Ln 1 Col 2");
    process.send(b"\x19");
    process.expect_body("ab", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// An open paste can be cancelled without treating Escape or Ctrl+Q bytes as keys.
#[test]
fn cancel_receiving_paste_with_signal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("paste.txt");
    std::fs::write(&path, b"original").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_status("Ready");
    process.send(b"\x1b[200~partial\x1b");
    process.expect_status("Receiving paste | kill -USR1");
    kill(process.editor_pid(), Signal::SIGUSR1).unwrap();
    process.expect_status("Cancelling paste");
    process.send(b"\x11discarded\x1b[201~");
    process.expect_status("Paste cancelled");
    process.send(b"\x1b[200~\x1b[27u\x11!\x1b[201~\x13");
    process.expect_status("Saved");
    assert_eq!(std::fs::read(&path).unwrap(), b"\x1b[27u\x11!original");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Probe backwards focus over CSI-u after the terminal advertises support.
#[test]
fn enhanced_shift_tab_moves_dialog_focus() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("focus.txt");
    std::fs::write(&path, b"original").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect_raw("\x1b[?u");
    process.send(b"\x1b[?1u");
    process.expect_raw("\x1b[>1u");
    process.expect("Ready");
    process.send(b"\x1b[14~");
    process.expect("Save As (new path)");
    process.send(b"\x1b[9;2u\rx");
    process.expect_body("xoriginal", TIMEOUT);
    process.send(b"\x13");
    process.expect_status("Saved");
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// Select a command through the portable keyboard navigation in Actions.
fn select_action(process: &mut PtyChild, category: &str, row: usize) {
    let index = [
        "File",
        "Edit",
        "Search",
        "Tabs",
        "Highlighting",
        "Streams",
        "Settings",
        "Help",
    ]
    .iter()
    .position(|label| *label == category)
    .unwrap();
    process.send(
        &[
            b"\x1bOQ".as_slice(),
            &b"\x1b[B".repeat(index),
            b"\x1b[C",
            &b"\x1b[B".repeat(row),
        ]
        .concat(),
    );
}

fn run_action(process: &mut PtyChild, category: &str, row: usize) {
    select_action(process, category, row);
    process.send(b"\r");
}

/// Assert terminal-visible attributes, including repaint as asynchronous work settles.
fn syntax_color(process: &mut PtyChild, line: u16, column: u16, color: vt100::Color) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        process.drain();
        if process
            .parser
            .screen()
            .cell(line, column)
            .unwrap()
            .fgcolor()
            == color
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "waiting for {color:?} at ({line}, {column}): {}",
            process.parser.screen().contents()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// K06: Safe startup keeps bundled syntax, reports the config path and rejects reload.
#[test]
fn safe_config_uses_bundled_theme_and_queries() {
    // Keep the escaped path on one rendered Help line on both platforms.
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let root = directory.path().join("cfg-\x1b");
    let config = root.join("ede");
    let queries = config.join("queries/rust/highlights.scm");
    std::fs::create_dir_all(queries.parent().unwrap()).unwrap();
    let highlighting = config.join("highlighting.toml");
    let general = config.join("config.toml");
    let theme = concat!(
        "default_theme = 'Custom'\n[themes.Custom]\nbase = 'dark'\n",
        "[themes.Custom.colors]\nkeyword = '#AA1234'\n"
    );
    let query = "(primitive_type) @number";
    let invalid_general = "[keybindings.global\nsave";
    std::fs::write(&highlighting, theme).unwrap();
    std::fs::write(&queries, query).unwrap();
    std::fs::write(&general, invalid_general).unwrap();
    let source = directory.path().join("source.rs");
    let bytes = b"fn example() { let a: u32 = 42; }\n";
    std::fs::write(&source, bytes).unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [std::ffi::OsStr::new("--safe-config"), source.as_os_str()],
        None,
        Some(&root),
        None,
    );
    process.expect_body("fn example()", TIMEOUT);
    process.expect_status("Safe config");
    syntax_color(&mut process, 1, 0, vt100::Color::Idx(14));
    syntax_color(&mut process, 1, 22, vt100::Color::Idx(11));
    run_action(&mut process, "Settings", 2); // Disable mouse capture.
    process.send(b"\x1bOP");
    process.expect(r"cfg-\u{1b}/ede/config.toml");
    process.send(b"\x1b");
    process.expect_absent("Up/Down scroll help");
    select_action(&mut process, "Highlighting", 2);
    process.expect("Reload Highlighting (disabled)");
    process.send(b"\r\x1b[B");
    process.expect(">Retry Highlighting<");
    process.send(b"\x1b");
    process.expect_absent("Retry Highlighting");
    syntax_color(&mut process, 1, 0, vt100::Color::Idx(14));
    syntax_color(&mut process, 1, 22, vt100::Color::Idx(11));
    process.send(b"X");
    process.expect_body("Xfn example()", TIMEOUT);
    process.send(b"\x11");
    process.expect("Save your changes before continuing?");
    process.send(b"\x1b[Z\r"); // Discard only via the visible confirmation.
    assert!(process.wait().success());
    process.assert_restored();
    assert_eq!(std::fs::read(source).unwrap(), bytes);
    assert_eq!(std::fs::read_to_string(highlighting).unwrap(), theme);
    assert_eq!(std::fs::read_to_string(queries).unwrap(), query);
    assert_eq!(std::fs::read_to_string(general).unwrap(), invalid_general);
}

/// Custom colors and query captures can address headings and list markers independently.
#[test]
fn markdown_theme_can_override_headings_and_list_markers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notes.md");
    let config = directory.path().join("ede");
    let queries = config.join("queries/markdown");
    std::fs::create_dir_all(&queries).unwrap();
    std::fs::write(&path, "# One\n## Two\n\n- Dash\n+ Plus\n\n1. Numbered\n").unwrap();
    let file = config.join("highlighting.toml");
    let base = concat!(
        "default_theme = 'Notes'\n[themes.Notes]\nbase = 'light'\n",
        "[themes.Notes.colors]\nmarkup = '#884411'\nheading_2 = '#225588'\n",
        "punctuation = '#664488'\n",
    );
    std::fs::write(&file, format!("{base}list_marker = '#116633'\n")).unwrap();
    std::fs::write(
        queries.join("highlights.scm"),
        "(list_marker_plus) @heading_2\n",
    )
    .unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&path],
        None,
        Some(directory.path()),
        None,
    );
    process.expect("## Two");
    syntax_color(&mut process, 1, 2, vt100::Color::Rgb(0x88, 0x44, 0x11));
    syntax_color(&mut process, 2, 3, vt100::Color::Rgb(0x22, 0x55, 0x88));
    syntax_color(&mut process, 4, 0, vt100::Color::Rgb(0x11, 0x66, 0x33));
    syntax_color(&mut process, 5, 0, vt100::Color::Rgb(0x22, 0x55, 0x88));
    syntax_color(&mut process, 7, 0, vt100::Color::Rgb(0x11, 0x66, 0x33));
    std::fs::write(&file, base.replace("#225588", "#334499")).unwrap();
    run_action(&mut process, "Highlighting", 2);
    syntax_color(&mut process, 4, 0, vt100::Color::Rgb(0x66, 0x44, 0x88));
    syntax_color(&mut process, 5, 0, vt100::Color::Rgb(0x33, 0x44, 0x99));
    syntax_color(&mut process, 7, 0, vt100::Color::Rgb(0x66, 0x44, 0x88));
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// H09: real fresh config, palette repaint, user query, failed reload, and reset.
#[test]
fn highlighting_configuration_reload_keeps_last_good_revision() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("ede");
    let rules = config.join("queries/rust/highlights.scm");
    std::fs::create_dir_all(rules.parent().unwrap()).unwrap();
    let file = config.join("highlighting.toml");
    let source = directory.path().join("source.demo");
    std::fs::write(&source, "fn example() { let a: u32 = 42; }\n").unwrap();
    let theme = |hex: &str| {
        format!(
            r##"
        default_theme = "Ocean"
        [themes.Ocean]
        base = "dark"
        [themes.Ocean.colors]
        keyword = "{hex}"
        number = "#04AB27"
        [files.extensions]
        Demo = "rust"
    "##
        )
    };
    std::fs::write(&file, theme("#AA1234")).unwrap();
    std::fs::write(&rules, "(primitive_type) @number").unwrap();
    let mut process = PtyChild::editor_with_options(
        Some("xterm-256color"),
        [&source],
        None,
        Some(directory.path()),
        None,
    );
    process.expect("fn example()");
    syntax_color(&mut process, 1, 0, vt100::Color::Rgb(0xaa, 0x12, 0x34));
    syntax_color(&mut process, 1, 22, vt100::Color::Rgb(4, 0xab, 0x27));
    std::fs::write(&file, theme("#556677")).unwrap();
    run_action(&mut process, "Highlighting", 2);
    syntax_color(&mut process, 1, 0, vt100::Color::Rgb(0x55, 0x66, 0x77));
    syntax_color(&mut process, 1, 22, vt100::Color::Rgb(4, 0xab, 0x27));

    std::fs::write(&rules, "(missing_node_kind) @keyword").unwrap();
    std::fs::write(&file, theme("#111111")).unwrap();
    run_action(&mut process, "Highlighting", 2);
    process.expect("Syntax config error");
    syntax_color(&mut process, 1, 0, vt100::Color::Rgb(0x55, 0x66, 0x77));
    run_action(&mut process, "Tabs", 2);
    process.expect("queries/rust/highlights.scm");
    process.send(b"\x1b");
    process.expect_absent("queries/rust/highlights.scm");
    std::fs::remove_file(&rules).unwrap();
    std::fs::remove_file(&file).unwrap();
    run_action(&mut process, "Highlighting", 2);
    process.expect_absent("Syntax config error");
    syntax_color(&mut process, 1, 0, vt100::Color::Default);
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// H04/H06: an invisible overlong line disables a complete revision, and
/// foreground text and replacement still work before refresh returns syntax.
#[test]
fn highlighting_line_fallback_preserves_editor_and_recovers_on_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("source.rs");
    let mut text = String::from("fn sample() {}\r\n");
    text.push_str(&"\r\n".repeat(100));
    text.push_str("//");
    text.push_str(&"x".repeat(64 * 1024 - 1));
    text.push_str("\r\n");
    std::fs::write(&file, text.as_bytes()).unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&file));
    process.expect("Plain syntax");
    process.expect("fn sample() {}");
    syntax_color(&mut process, 1, 0, vt100::Color::Default);
    run_action(&mut process, "Tabs", 2);
    process.expect("a line exceeds 64 KiB");
    process.send(b"\x1b");
    process.expect_absent("a line exceeds 64 KiB");
    process.send(b"\x06sample\r");
    process.expect("1 match");
    process.send(b"\x1b");
    std::fs::write(&file, b"fn updated() {}\r\n").unwrap();
    process.expect("fn updated() {}");
    process.expect_absent("Plain syntax");
    syntax_color(&mut process, 1, 0, vt100::Color::Idx(14));
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
    assert_eq!(std::fs::read(&file).unwrap(), b"fn updated() {}\r\n");
}
/// H05: parking highlighted tabs releases their projection charges.
#[test]
fn sixty_four_highlighted_tabs_keep_foreground_budget() {
    let directory = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..64)
        .map(|index| {
            let path = directory.path().join(format!("s{index:02}.md"));
            std::fs::write(&path, format!("# Heading {index:02}\n")).unwrap();
            path
        })
        .collect();
    let mut process =
        PtyChild::editor_with_options(Some("xterm-256color"), &paths, None, None, Some(256));
    for index in 0..64 {
        process.expect_body(&format!("# Heading {index:02}"), TIMEOUT);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            process.drain();
            if process.parser.screen().cell(1, 2).unwrap().fgcolor() == vt100::Color::Idx(13) {
                break;
            }
            if Instant::now() >= deadline {
                let screen = process.parser.screen().contents();
                run_action(&mut process, "Tabs", 2);
                process.expect("Highlighting:");
                panic!(
                    "tab {index} stopped highlighting: {screen}\n{}",
                    process.parser.screen().contents()
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
        if index < 63 {
            process.send(b"\x1b[19~");
        }
    }
    process.send(b"fresh \x13");
    process.expect("fresh # Heading 63");
    process.expect_status("Saved");
    assert_eq!(
        std::fs::read_to_string(&paths[63]).unwrap(),
        "fresh # Heading 63\n"
    );
    process.send(b"\x11");
    let status = process.wait();
    assert!(
        status.success(),
        "{status}: {}",
        String::from_utf8_lossy(&process.output)
    );
    process.assert_restored();
}

/// H08: exercise both the size fallback and real parser work while search scans a parked 1 GiB tab.
#[test]
#[ignore = "allocates a large file and samples two live processes"]
fn large_highlighting_and_search_overlap() {
    let directory = tempfile::tempdir().unwrap();
    let syntax = directory.path().join("syntax.rs");
    let large = directory.path().join("oversize.rs");
    let mut writer = File::create(&syntax).unwrap();
    writer.write_all(b"fn LEADING_TOKEN() {}\n").unwrap();
    let comment = format!("// {}\n", "x".repeat(40 * 1024 - 4));
    for _ in 0..510 {
        writer.write_all(comment.as_bytes()).unwrap();
    }
    writer.sync_all().unwrap();
    drop(writer);
    let mut writer = File::create(&large).unwrap();
    let block = b"ordinary-search-data\n".repeat(1 << 15);
    let mut size = 0u64;
    let mut synced = 0u64;
    while size < 1 << 30 {
        let bytes = (block.len() as u64).min((1 << 30) - size) as usize;
        writer.write_all(&block[..bytes]).unwrap();
        size += bytes as u64;
        if size - synced >= 8 << 20 {
            writer.sync_data().unwrap();
            #[cfg(target_os = "linux")]
            nix::fcntl::posix_fadvise(
                &writer,
                synced as i64,
                (size - synced) as i64,
                nix::fcntl::PosixFadviseAdvice::POSIX_FADV_DONTNEED,
            )
            .unwrap();
            synced = size;
        }
    }
    writer.sync_all().unwrap();
    drop(writer);
    eprintln!(
        "highlight_search fixtures_ready active_bytes={} inactive_bytes={size}",
        std::fs::metadata(&syntax).unwrap().len()
    );

    let started = Instant::now();
    let mut process = PtyChild::editor_with_args(Some("xterm-256color"), [&syntax, &large]);
    process.expect_body("fn LEADING_TOKEN()", Duration::from_secs(10));
    let first = started.elapsed();
    process.send(b"\x1b[19~");
    process.expect_body("ordinary-search-data", Duration::from_secs(120));
    process.expect_with_timeout("Plain syntax", Duration::from_secs(120));
    process.send(b"\x1b[18~");
    process.expect_body("fn LEADING_TOKEN()", Duration::from_secs(10));
    process.send(b"\x06ABSENT_MARKER");
    process.click_label("Options", 1);
    process.click_label("[Current tab]", 1);
    process.click_label("[Find]", 1);
    process.expect("Searching");

    let pid = process.editor_pid().as_raw();
    let mut peak_editor = 0;
    let mut peak_worker = 0;
    let mut worker_sample = None;
    let mut overlap = false;
    let mut max_input = Duration::ZERO;
    let detect = Instant::now();
    while Instant::now() - detect < Duration::from_secs(10) {
        process.drain();
        let screen = process.parser.screen().contents();
        if !screen.contains("Searching") {
            break;
        }
        let output = Command::new("ps")
            .args(["-ax", "-o", "pid=,ppid=,rss=,state=,time="])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "ps: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut fields = line.split_whitespace();
            let (Some(child), Some(parent), Some(rss)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let rss = rss.parse::<u32>().unwrap();
            if child.parse::<i32>() == Ok(pid) {
                peak_editor = peak_editor.max(rss);
            }
            if parent.parse::<i32>() == Ok(pid) {
                peak_worker = peak_worker.max(rss);
                let state = fields.next().unwrap_or_default();
                let now = fields.next().unwrap_or_default();
                let used_cpu =
                    worker_sample
                        .as_ref()
                        .is_some_and(|(old_pid, old_time): &(String, String)| {
                            old_pid == child && old_time != now
                        });
                overlap |=
                    screen.contains("/1073741824 bytes") && (state.starts_with('R') || used_cpu);
                worker_sample = Some((child.into(), now.into()));
            }
        }
        thread::sleep(Duration::from_millis(3));
    }
    assert!(
        overlap,
        "no active parser CPU sampled during the 1 GiB scan"
    );
    process.send(b"\x1b[17~"); // F6 transfers focus to the document.
    for count in 1..=8 {
        let event = Instant::now();
        process.send(b" ");
        process.expect_body(&format!("{}fn LEADING_TOKEN", " ".repeat(count)), TIMEOUT);
        max_input = max_input.max(event.elapsed());
    }
    process.send(b"\x1b");
    process.expect_absent("Scope: All open");
    process.send(b"\x13");
    process.expect_with_timeout("Saved", Duration::from_secs(30));
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
    eprintln!(
        "highlight_search first_text_ms={:.3} max_input_ms={:.3} editor_sample_kib={peak_editor} worker_sample_kib={peak_worker}",
        first.as_secs_f64() * 1000.0,
        max_input.as_secs_f64() * 1000.0
    );
    process.report_peak_rss();
}

/// H06: projected source style covers the entire grapheme and yields to selection and search.
#[test]
fn syntax_color_yields_to_selection_and_current_match() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("style.md");
    std::fs::write(&path, "# Note 界e\u{301}\tend\n").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&path));
    process.expect("# Note 界e\u{301}");
    syntax_color(&mut process, 1, 2, vt100::Color::Idx(13));
    syntax_color(&mut process, 1, 7, vt100::Color::Idx(13));
    process.send(b"\x01");
    syntax_color(&mut process, 1, 2, vt100::Color::Idx(15));
    assert_eq!(
        process.parser.screen().cell(1, 7).unwrap().bgcolor(),
        vt100::Color::Idx(4)
    );
    process.send(b"\x06Note\r");
    process.expect("1 match");
    syntax_color(&mut process, 1, 2, vt100::Color::Idx(0));
    assert_eq!(
        process.parser.screen().cell(1, 2).unwrap().bgcolor(),
        vt100::Color::Idx(5)
    );
    process.send(b"\x1b");
    process.expect_absent("Find:");
    process.send(b"\x11");
    assert!(process.wait().success());
    process.assert_restored();
}

/// Mouse activation should keep the item under the first click at the same screen position.
#[test]
fn picker_double_click_keeps_scrolled_rows_stationary() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.txt");
    std::fs::write(&original, "original").unwrap();
    for index in 0..26 {
        let path = directory.path().join(format!("entry-{index:02}.txt"));
        std::fs::write(path, format!("unique document bytes {index}")).unwrap();
    }
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&original));
    process.expect_status("Ready");
    process.send(b"\x0f");
    process.expect("entry-00.txt");
    process.send(b"\x1b[B".repeat(20).as_slice());
    process.expect("entry-20.txt");
    process.click_label("entry-10.txt", 2);
    process.expect_body("unique document bytes 10", TIMEOUT);
    process.send(b"\x11");
    assert!(process.wait().success());
}

/// A pointer release on another row interrupts the earlier click even without a drag event.
#[test]
fn picker_mismatched_release_interrupts_double_click() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.txt");
    std::fs::write(&original, "original").unwrap();
    std::fs::write(
        directory.path().join("first.txt"),
        "unexpected first file payload",
    )
    .unwrap();
    std::fs::write(directory.path().join("second.txt"), "second file payload").unwrap();
    let mut process = PtyChild::editor_with_path(Some("xterm-256color"), Some(&original));
    process.expect_status("Ready");
    process.send(b"\x0f");
    process.expect("second.txt");
    let coord = |label: &str| {
        process
            .parser
            .screen()
            .rows(0, 80)
            .enumerate()
            .find_map(|(row, text)| {
                text.find(label).map(|start| {
                    (
                        unicode_width::UnicodeWidthStr::width(&text[..start]) + 1,
                        row + 1,
                    )
                })
            })
            .unwrap()
    };
    let (x1, y1) = coord("first.txt");
    let (x2, y2) = coord("second.txt");
    let click = format!("\x1b[<0;{x1};{y1}M\x1b[<0;{x1};{y1}m");
    let mismatch = format!("\x1b[<0;{x1};{y1}M\x1b[<0;{x2};{y2}m");
    process.send(format!("{click}{mismatch}{click}").as_bytes());
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        process.drain();
        let screen = process.parser.screen().contents();
        assert!(
            !screen.contains("unexpected first file payload"),
            "{screen}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    process.expect("Open file");
    process.send(b"\x1b");
    process.expect_absent("Open file");
    process.send(b"\x11");
    assert!(process.wait().success());
}
