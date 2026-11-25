# ede

A Rust terminal text editor for macOS and Linux.

## Install from crates.io

Building ede requires Rust 1.100.0 or later and a C toolchain for the bundled
syntax grammars. Install the Xcode Command Line Tools on macOS or `build-essential`
on Ubuntu. To install the published crate, run the following commands.

```sh
cargo install ede
ede --version
```

## Build and run

Install Rust through rustup. `rust-toolchain.toml` pins the toolchain and its
formatting and linting components. Use the same C toolchain as for a crates.io
install.

```sh
cargo build --locked
cargo run --locked
cargo run --locked -- path/to/file.txt
cargo run --locked -- first.txt second.txt
cargo run --locked -- -- -filename.txt
producer | cargo run --locked
cargo run --locked < path/to/input.txt
cargo run --locked -- --follow path/to/current.log
cargo run --locked -- --safe-config path/to/file.txt
```

Run in a terminal with a usable `TERM`, at least 40 columns by 8 rows. Smaller
windows preserve work and show a resize prompt; return to a supported size to
answer unsaved-change prompts. `--help` and `--version` work without a terminal.

A launch without a file or retained scratch opens a welcome screen. Choose
**Open File** (Ctrl+O) or **New Scratch** (Ctrl+N). File-open errors are shown
in the session. Existing scratch documents restore on launch.

Start with `--safe-config` to use bundled configuration for the session. The
option comes before file arguments, `--`, or `--follow`. Safe mode skips local
highlighting overrides and query files and disables Reload Highlighting. Press
F1 to see the expected config path. Editing and scratch recovery work as usual.

Pipe, FIFO, and regular-file stdin open a read-only **Stdin** tab ahead of any
named files. Use a foreground controlling terminal and leave stdout attached to
a terminal. A clean text EOF converts the same tab into an editable, recoverable
scratch. `--follow FILE` requires terminal stdin and accepts exactly one path.
**Menu → Streams → Follow File** opens the same kind of read-only follower from the picker.

Live tabs open at the tail. **Pause View** keeps the current viewport while
ingestion continues. **Resume Follow** returns to the tail. Find searches the
retained history, and moving away or selecting text pauses the view. Followers
track pathname replacement and common truncations. Tab Details reports the
latest detected boundary. Up to four live sources share an 8 GiB payload cap,
with a 2 GiB cap per source. A stopped capture keeps its committed prefix and
offers **Export Retained Copy** to a new path. A failed EOF conversion also
offers **Retry Finish**.

Opening validates a private UTF-8 snapshot in the background. Its initial preview
is read-only; Escape cancels preparation. Once ready, files remain editable
regardless of total size. Lines do not wrap; horizontal scrolling follows the
caret. Enter uses the document's first LF or CRLF, or the configured default if
the document has no line endings. Tab inserts a
literal tab, and existing bytes are not normalized. Unsupported encodings and
binary data report errors.

## Controls

| Action | Portable shortcut | Additional shortcut when Cmd is delivered |
| --- | --- | --- |
| New / Open | Ctrl+N / Ctrl+O | Cmd+N / Cmd+O |
| Previous / next tab | F7 / F8 | Same |
| Close tab | Ctrl+W | Cmd+W |
| Save / Save As | Ctrl+S / F4 | Cmd+S / Cmd+Shift+S |
| Quit | Ctrl+Q | Cmd+Q |
| Select all | Ctrl+A | Cmd+A |
| Cut / Copy / internal paste | Ctrl+X / Ctrl+C / Ctrl+V | Cmd+X / Cmd+C / Cmd+V |
| Undo / Redo | Ctrl+Z / Ctrl+Y | Cmd+Z / Cmd+Shift+Z |
| Move / extend selection | Arrows, Home, End, Page Up/Down; Shift extends | Same |
| Document start / end | Ctrl+Home / Ctrl+End | Same |
| Go to line or line:cell | Ctrl+G; numbers start at 1 | Same |
| Find / Replace | Ctrl+F / Ctrl+H | Cmd+F / Cmd+H |
| Next / previous match | F3 / Shift+F3 | Same |
| Help / actions | F1 / F2 | Same |
| Leave text / previous control | F6 / Shift+F6 | Same |
| Activate / cancel | Enter / Escape | Same |

Buttons and the action menu provide alternatives for shortcuts intercepted by a
terminal. Tab cycles controls in dialogs/help and inserts text in the editor.
Help scrolls with Up/Down or the wheel. **Menu** (F2) opens Actions with categories
on the left and commands on the right. Use Left/Right or Tab to move between
columns, Up/Down to navigate, and Enter to activate. The wheel scrolls the
column under the pointer. Suspend is under **Menu → Help** and in the Help pane.
Resume with the shell's `fg` command.

New and Open add tabs, and switching restores each tab's caret, selection,
scrolling, and undo history. Use the Tabs button or **Menu → Tabs → Tabs** to
switch to or close a tab outside the strip. In the Tabs pane, `>` marks the active
tab and highlighting follows the selected row. **Details** inspects the selected
tab without switching. **Close Selected Tab** starts the normal close checks for
the selected row. **Back** dismisses the pane. **Menu → Tabs → Tab details** opens
details for the active tab. Dividers separate tabs in the strip, where reverse
video marks the active tab. The strip also marks modified (`*`), waiting (`…`), loading
(`↓`), queued save (`⇡`), saving (`↑`), scratch (`~`), and attention (`!`).

**Find** and **Find & Replace** open in compact windows over the editor. Enter
the query and press Enter or click **Find**. Expand **Options** with a click,
Enter, or Right to change **Match case**, **Match Whole Word**, or the search
scope. All open includes unsaved text. Queries are literal, with optional
Unicode case folding. Fields accept up to 4096 UTF-8 bytes, including pasted
Tab, LF and CRLF. The expanded options
also insert those characters directly. Portable Backspace uses DEL (`0x7f`).
Ctrl+H (`0x08`) opens Find & Replace.

Use Up and Down or Tab and Shift+Tab to move through the form. Click a result
or use the arrow keys in the result list to jump to its exact match. Page Up,
Page Down, Home and End browse the list. The mouse wheel scrolls it without
moving the caret. Left returns to the form and F6 returns to editor text.
F3 and Shift+F3 wrap through live matches.

**Replace Next** reveals a match before changing it and advances without
wrapping. **Replace All** confirms the scope with Cancel focused. **Replaced**
and **Removed** mark the latest committed changes in the result list. Each
changed document gets one Undo step, and saving remains a separate action.
Search Summary reports per-tab outcomes. Escape cancels active work and closes
the idle window. Search navigation clears ordinary selection and keeps the
caret on a grapheme boundary. The exact-match highlight shows the replacement
range.

Open starts in the active file's parent directory or the launch directory.
Use arrows or the wheel to select an entry and Enter to open it. Double-click
a directory to browse it or a file to open it. Up and the page buttons browse
directories. Tab to Path to enter a literal absolute or relative path. The
listing includes hidden entries.

Click to place the caret, Shift-click to extend, or drag to select. Dragging beyond
the text area autoscrolls while the terminal continues reporting pointer events.
The wheel scrolls the view without moving the caret. **Menu → Settings → Toggle mouse
capture** pauses capture for terminal-native selection; F2 gives a keyboard route
to turn it back on. Native-selection modifiers depend on the terminal; use tmux's
copy mode inside tmux.

With mouse capture enabled, click outside a pane to dismiss it. This includes
the Find and Find & Replace windows. The click leaves the editor caret and
underlying controls alone. Confirmations and guarded Save As require an explicit choice. Click a tab
name in the header to switch or its × to close that tab. Each close uses the same
save and discard checks as keyboard Close Tab.

Copy retains the full selection for the session, including text outside the
viewport. Copy and Cut use the desktop clipboard in an identified local GUI
session. Cut waits for the desktop handoff before deleting the selection.
Ctrl+V reads the current desktop clipboard on that route. Empty or unreadable
clipboard data leaves the document intact.

SSH, tmux, other remote transports, and unidentified sessions send an OSC 52
write and use ede's retained selection for Ctrl+V. Requests over 8,192 UTF-8 bytes
are skipped, and the full selection remains available in ede. A sent request
means the tty accepted the frame. The terminal decides whether to deliver it.
The Ubuntu desktop route uses X11 CLIPBOARD through Xorg or through the GNOME
Wayland session's Xwayland clipboard bridge.

The terminal's own Paste command sends client text through bracketed paste.
All document pastes convert LF, CRLF, and standalone CR to the line ending
Enter would insert. The document's first line ending takes precedence. For a
new or open document with no LF or CRLF, **Settings → Editor → Default line
ending** selects LF or CRLF, with LF as the default. The change applies as soon
as you save Settings. In `config.toml`, use `default_line_ending = "lf"` or
`"crlf"` under `[editor]`.
**Menu → Tabs → Tab details** shows the line ending the tab will use.
For example, on macOS Cmd+V typically belongs to the terminal. All paste paths
support large contents using disk staging. A paste commits as one undoable edit
or leaves the document unchanged on validation, staging, space, or pre-commit
cancellation failure. Bytes within bracketed paste are always data, including
Escape. While the status shows `Receiving paste`, run the displayed `kill -USR1 PID` command
from another shell on the editor host to cancel reception. The reader discards
the rest of that frame through its closing marker. Escape can cancel an admitted
paste before it commits once the terminal has sent the closing marker.

Set `clipboard.mode = "internal"` under `[clipboard]` to keep editor clipboard
operations private. The legacy `copy_to_system = false` and `--safe-config`
also select private operation. A missing setting, legacy `"system"` or
`"both"`, or `copy_to_system = true` select automatic routing.

For iTerm2, check its clipboard write permission. An intervening tmux server
needs to accept writes from programs inside a pane and forward them to the
client. Its `set-clipboard external` mode can forward tmux copy-mode selections
while ignoring application writes. When the terminal path cannot accept OSC 52,
use ede's internal clipboard, terminal-native selection with mouse capture
paused, or tmux copy mode. See the
[iTerm2 clipboard settings](https://iterm2.com/documentation-preferences-general.html)
and [tmux clipboard documentation](https://github.com/tmux/tmux/wiki/Clipboard).

When clipboard writes are unavailable in Apple Terminal, you can copy text
visible on screen using terminal-native selection. Press **F2** and toggle mouse
capture off, drag to select the text, and press **Option+Shift+Cmd+C** to copy
plain text. Toggle capture back on to restore clicks in ede. Apple Terminal also
offers **View → Allow Mouse Reporting**. Use **Cmd+V** or **Edit → Paste** to send
Mac clipboard text into ede.

Save As creates a new destination; it does not overwrite an existing path. Normal
Save checks the previously acknowledged destination for external changes. Editing
can continue during saving; a save of older content leaves newer edits unsaved.
Close protects every scratch, including empty ones, with Save / Discard / Cancel
and Cancel selected initially. Quit automatically retains scratches and checks
modified named tabs in creation order. Canceling a later prompt keeps the session.
An uncertain save explicitly reports that the destination may have changed.

Scratches checkpoint every five seconds and flush their final contents on normal
Quit. Relaunch restores text, tab order, active scratch, caret, selection, and
viewport with fresh undo history. Each running instance owns a separate scratch
set; a new instance claims scratches from exited or crashed instances. Excess
entries beyond 64 open tabs remain in **Menu → File → Recovery**, with Open, Retry,
Discard, and paging. A crash restores the last complete checkpoint; unsaved
named-file edits are outside this recovery scope.

Recovery state lives in `$HOME/Library/Application Support/ede` on macOS, or
`$XDG_STATE_HOME/ede` on Linux (falling back to `$HOME/.local/state/ede`). Set an
absolute `EDE_STATE_DIR` to use another profile. Unusable state or a corrupt
catalog stops startup with a diagnostic. Failed objects remain listed with their
location and error so they can be repaired and retried. Persistence errors offer
Retry, Save As, and Cancel; failed Close retries Close. Tab details keeps the
checkpoint status and unresolved error visible.

Clean named tabs refresh automatically after external saves, including inactive
tabs and rename saves. Local edits and paste reception prevent replacement;
guarded Save still checks for conflicts. Refresh preserves the latest view and
adds one undo step within the 256 MiB history budget. If the previous text exceeds
that budget, refresh resets history and reports it. Missing or invalid sources
leave the last good text intact and retry with backoff. **Menu → File → Retry refresh**
and Tab details provide an immediate retry.

## Configuration and keybindings

ede looks for `config.toml` under `$XDG_CONFIG_HOME/ede` when
`XDG_CONFIG_HOME` is absolute, or under `$HOME/.config/ede` when HOME is
absolute. A remote session uses the environment of the host running ede.
Configuration is optional. `highlighting.toml` and `queries/` in the same
directory still control custom themes, language rules, and queries.

```toml
[recovery]
checkpoint_interval_seconds = 5

[highlighting]
max_document_bytes = 20971520
max_line_bytes = 65536
parse_timeout_ms = 250

[keybindings.global]
save = ["Ctrl+S", "F9", "Cmd+S"]
reload_settings = []

[keybindings.picker]
activate = ["Enter", "Ctrl+O"]
```

The checkpoint interval accepts 1–3,600 seconds. Highlighting accepts 1–20,971,520
logical document bytes, 1–65,536 bytes per logical line, and 1–250 ms per
analysis. The example shows the scalar defaults. BOM bytes are excluded from
logical text, and a CRLF's CR does not count toward its line limit.

Every binding list replaces that command's defaults in its context. Leave the
entry out to inherit defaults or set it to `[]` to unbind it. Each entry accepts
up to eight chords. Unrecognized fields, conflicting chords and settings larger
than 64 KiB are rejected together. Workspace contexts are `editor`, `controls`
and `search`. They share eligible `global` bindings. The `picker`, `dialog`
and `settings` contexts own input while they are on top. Keep portable routes
to Actions and Quit, and through each surface's focus, Activate and Cancel.

Modifiers are Ctrl, Alt, Shift and Cmd in canonical display order. Modifier and
key names are ASCII case-insensitive. `Super` is accepted as `Cmd`. Supported
keys are modified ASCII letters, digits and Space, plus Escape, Enter, Tab,
Backspace, Delete, Left, Right, Up, Down, Home, End, PageUp, PageDown, and F1–F12.
Plain and Shift-only text keys remain available for typing. Some enhanced keys
depend on terminal delivery. Legacy Ctrl+I is Tab, Ctrl+J and Ctrl+M are Enter,
and byte 08 is Ctrl+H. A Backspace key must send DEL to work as Backspace on the
legacy path. See [Cmd delivery](#cmd-delivery).

Click **Settings** or press **F5**. **Menu → Settings → Settings** and keyboard focus
also open it. Assigning F5 explicitly to another workspace command suppresses
the inherited Settings shortcut until that conflict is removed. Explicit
Settings bindings and unbindings take precedence.

Choose **Recovery**, **Highlighting Limits**, **Editor**, or **Keyboard Shortcuts**
in the sidebar. Narrow terminals use **Recovery**, **Limits**, **Editor**, and
**Keys** above the list. Use Tab or Shift+Tab to focus the section selectors or
rows. Move between sections with Up/Down in the sidebar and Left/Right in the
compact layout.

**Keyboard Shortcuts** expands into groups for the screen or control that has
focus. **App commands** covers workspace actions such as Open file and Open
Settings. **Focused buttons** covers keys such as Enter and Tab when a workspace
button has focus. **While in Settings** covers actions within the Settings pane
itself. When groups appear above the list, use the arrows beside the group name
to change groups.
The **Command** and **Shortcut** columns show each binding. Click a row or press
Enter on it to edit. Type a chord or choose **Capture** and accept the preview.
The binding editor title shows the configuration ID. All sections share one
draft. At 40×8, Reload and Restore Defaults are available from the Recovery
and Limits sections.

The bottom-right **Save** includes the current typed value. **Discard** restores
applied values. **Exit** or an outside click opens a confirmation when the draft
has changes. **Keep Editing** or Escape returns to the draft. **Reload** reads
the file and **Restore Defaults** drafts the bundled values, including removal
of a clipboard opt-out. Changes apply when saved. Settings shows the active
configuration source and diagnostics. Open **Details** when a diagnostic is
truncated. **Menu → Help → About** shows the app version and configuration file path.

Settings saves preserve unrelated TOML and comments. If the file changes
externally, reload it before saving a new draft. Any noncooperating writer can
race the final filesystem check and rename of an existing file. A save reported
as uncertain requires a reload to see what reached disk. **Use defaults for
this session** or Escape bypasses a slow initial load. A rejected initial file
uses defaults and keeps a diagnostic. A rejected reload retains the previous
settings. Safe mode disables saving and reloading.

### Command IDs and default shortcuts

The following rows list stable IDs within each context. An em dash indicates
that a command starts unbound and is available when its surface permits it.

<!-- keymap-defaults:start -->
| Context | Command ID | Defaults |
| --- | --- | --- |
| `global` | `about` | — |
| `global` | `actions` | F2 |
| `global` | `cancel_search` | — |
| `global` | `close_tab` | Ctrl+W / Cmd+W |
| `global` | `export_retained` | — |
| `global` | `find` | Ctrl+F / Cmd+F |
| `global` | `find_next` | F3 |
| `global` | `find_previous` | Shift+F3 |
| `global` | `follow_file` | — |
| `global` | `go_to` | Ctrl+G |
| `global` | `help` | F1 |
| `global` | `language` | — |
| `global` | `mouse_capture` | — |
| `global` | `new` | Ctrl+N / Cmd+N |
| `global` | `next_tab` | F8 |
| `global` | `open` | Ctrl+O / Cmd+O |
| `global` | `previous_tab` | F7 |
| `global` | `quit` | Ctrl+Q / Cmd+Q |
| `global` | `recovery` | — |
| `global` | `reload_highlighting` | — |
| `global` | `reload_settings` | — |
| `global` | `replace` | Ctrl+H / Shift+Cmd+H |
| `global` | `replace_all` | — |
| `global` | `replace_next` | — |
| `global` | `retry_finish` | — |
| `global` | `retry_highlighting` | — |
| `global` | `retry_refresh` | — |
| `global` | `save` | Ctrl+S / Cmd+S |
| `global` | `save_as` | Shift+Cmd+S / F4 |
| `global` | `settings` | F5 |
| `global` | `suspend` | — |
| `global` | `syntax_theme` | — |
| `global` | `tab_details` | — |
| `global` | `tabs` | — |
| `global` | `toggle_follow` | — |
| `editor` | `backspace` | Backspace |
| `editor` | `cancel` | Escape |
| `editor` | `copy` | Ctrl+C / Cmd+C |
| `editor` | `cut` | Ctrl+X / Cmd+X |
| `editor` | `delete` | Delete |
| `editor` | `focus_next` | — |
| `editor` | `focus_previous` | Shift+Tab / Shift+F6 |
| `editor` | `focus_search` | F6 |
| `editor` | `insert_newline` | Enter |
| `editor` | `insert_tab` | Tab |
| `editor` | `internal_paste` | Ctrl+V / Cmd+V |
| `editor` | `move_document_end` | Ctrl+End |
| `editor` | `move_document_start` | Ctrl+Home |
| `editor` | `move_down` | Down |
| `editor` | `move_end` | End |
| `editor` | `move_home` | Home |
| `editor` | `move_left` | Left |
| `editor` | `move_page_down` | PageDown |
| `editor` | `move_page_up` | PageUp |
| `editor` | `move_right` | Right |
| `editor` | `move_up` | Up |
| `editor` | `redo` | Ctrl+Y / Shift+Cmd+Z |
| `editor` | `select_all` | Ctrl+A / Cmd+A |
| `editor` | `select_document_end` | Ctrl+Shift+End |
| `editor` | `select_document_start` | Ctrl+Shift+Home |
| `editor` | `select_down` | Shift+Down |
| `editor` | `select_end` | Shift+End |
| `editor` | `select_home` | Shift+Home |
| `editor` | `select_left` | Shift+Left |
| `editor` | `select_page_down` | Shift+PageDown |
| `editor` | `select_page_up` | Shift+PageUp |
| `editor` | `select_right` | Shift+Right |
| `editor` | `select_up` | Shift+Up |
| `editor` | `undo` | Ctrl+Z / Cmd+Z |
| `controls` | `activate` | Enter |
| `controls` | `cancel` | Escape |
| `controls` | `focus_next` | Tab / F6 |
| `controls` | `focus_previous` | Shift+Tab / Shift+F6 |
| `search` | `activate` | Enter |
| `search` | `cancel` | Escape |
| `search` | `close_search` | — |
| `search` | `field_backspace` | Backspace |
| `search` | `focus_editor` | F6 / Shift+F6 |
| `search` | `focus_next` | Tab |
| `search` | `focus_previous` | Shift+Tab |
| `search` | `insert_query_crlf` | — |
| `search` | `insert_query_lf` | — |
| `search` | `insert_query_tab` | — |
| `search` | `move_document_end` | Ctrl+End / Ctrl+Shift+End |
| `search` | `move_document_start` | Ctrl+Home / Ctrl+Shift+Home |
| `search` | `move_down` | Down / Shift+Down |
| `search` | `move_end` | End / Shift+End |
| `search` | `move_home` | Home / Shift+Home |
| `search` | `move_left` | Left / Shift+Left |
| `search` | `move_page_down` | PageDown / Shift+PageDown |
| `search` | `move_page_up` | PageUp / Shift+PageUp |
| `search` | `move_right` | Right / Shift+Right |
| `search` | `move_up` | Up / Shift+Up |
| `search` | `search_details` | — |
| `search` | `toggle_match_case` | — |
| `search` | `toggle_search_scope` | — |
| `search` | `toggle_whole_word` | — |
| `picker` | `activate` | Enter |
| `picker` | `cancel` | Escape |
| `picker` | `field_backspace` | Backspace |
| `picker` | `focus_next` | Tab / F6 |
| `picker` | `focus_previous` | Shift+Tab / Shift+F6 |
| `picker` | `move_document_end` | Ctrl+End / Ctrl+Shift+End |
| `picker` | `move_document_start` | Ctrl+Home / Ctrl+Shift+Home |
| `picker` | `move_down` | Down / Shift+Down |
| `picker` | `move_end` | End / Shift+End |
| `picker` | `move_home` | Home / Shift+Home |
| `picker` | `move_left` | Left / Shift+Left |
| `picker` | `move_page_down` | PageDown / Shift+PageDown |
| `picker` | `move_page_up` | PageUp / Shift+PageUp |
| `picker` | `move_right` | Right / Shift+Right |
| `picker` | `move_up` | Up / Shift+Up |
| `picker` | `picker_next` | — |
| `picker` | `picker_parent` | — |
| `picker` | `picker_previous` | — |
| `picker` | `select_row` | — |
| `dialog` | `activate` | Enter |
| `dialog` | `cancel` | Escape |
| `dialog` | `close_selected_tab` | — |
| `dialog` | `confirm_replace_all` | — |
| `dialog` | `discard` | — |
| `dialog` | `field_backspace` | Backspace |
| `dialog` | `focus_next` | Tab / F6 |
| `dialog` | `focus_previous` | Shift+Tab / Shift+F6 |
| `dialog` | `move_document_end` | Ctrl+End / Ctrl+Shift+End |
| `dialog` | `move_document_start` | Ctrl+Home / Ctrl+Shift+Home |
| `dialog` | `move_down` | Down / Shift+Down |
| `dialog` | `move_end` | End / Shift+End |
| `dialog` | `move_home` | Home / Shift+Home |
| `dialog` | `move_left` | Left / Shift+Left |
| `dialog` | `move_page_down` | PageDown / Shift+PageDown |
| `dialog` | `move_page_up` | PageUp / Shift+PageUp |
| `dialog` | `move_right` | Right / Shift+Right |
| `dialog` | `move_up` | Up / Shift+Up |
| `dialog` | `recovery_discard` | — |
| `dialog` | `recovery_next` | — |
| `dialog` | `recovery_open` | — |
| `dialog` | `recovery_previous` | — |
| `dialog` | `recovery_retry` | — |
| `dialog` | `retry` | — |
| `dialog` | `retry_refresh` | — |
| `dialog` | `save` | — |
| `dialog` | `save_as` | F4 / Shift+Cmd+S |
| `dialog` | `select_row` | — |
| `settings` | `accept_setting` | — |
| `settings` | `activate` | Enter |
| `settings` | `add_shortcut` | — |
| `settings` | `cancel` | Escape |
| `settings` | `cancel_save` | — |
| `settings` | `capture_shortcut` | — |
| `settings` | `discard_capture` | — |
| `settings` | `discard_draft` | — |
| `settings` | `edit_setting` | — |
| `settings` | `field_backspace` | Backspace |
| `settings` | `focus_next` | Tab / F6 |
| `settings` | `focus_previous` | Shift+Tab / Shift+F6 |
| `settings` | `keep_editing` | — |
| `settings` | `move_document_end` | Ctrl+End / Ctrl+Shift+End |
| `settings` | `move_document_start` | Ctrl+Home / Ctrl+Shift+Home |
| `settings` | `move_down` | Down / Shift+Down |
| `settings` | `move_end` | End / Shift+End |
| `settings` | `move_home` | Home / Shift+Home |
| `settings` | `move_left` | Left / Shift+Left |
| `settings` | `move_page_down` | PageDown / Shift+PageDown |
| `settings` | `move_page_up` | PageUp / Shift+PageUp |
| `settings` | `move_right` | Right / Shift+Right |
| `settings` | `move_up` | Up / Shift+Up |
| `settings` | `reload_settings` | — |
| `settings` | `remove_shortcut` | — |
| `settings` | `replace_shortcut` | — |
| `settings` | `restore_defaults` | — |
| `settings` | `save_settings` | — |
| `settings` | `select_row` | — |
| `settings` | `settings_info` | — |
| `settings` | `settings_options` | — |
| `settings` | `use_defaults` | — |
<!-- keymap-defaults:end -->

## Syntax highlighting

Open a supported file to color its editable source. **F2 → Language…** selects
Auto, Plain Text, or a language for the current tab. **F2 → Syntax Theme…**
chooses a session-wide palette. Tab Details shows the language and highlighting
state. If a file exceeds the size, line, time, or worker memory limit, the whole
revision uses plain text. Use **Retry Highlighting** to retry an analysis failure.
The terminal's `NO_COLOR` setting suppresses colors.

Local overrides live in `$XDG_CONFIG_HOME/ede` when `XDG_CONFIG_HOME` is an
absolute path, or `~/.config/ede` otherwise. Add `highlighting.toml`, then choose
**F2 → Reload Highlighting**. Reload applies the complete configuration once it
validates. Errors appear in Tab Details and the last good configuration stays active.

```toml
default_theme = "Ocean"

[themes.Ocean]
base = "dark"

[themes.Ocean.colors]
keyword = "#68D5F7"
comment = "bright-black"
heading_2 = "bright-cyan"
list_marker = "bright-yellow"
ordered_list_marker = "bright-cyan"
quote_marker = "bright-blue"

[files.names]
".env" = "ini"

[files.extensions]
notes = "markdown"

[fences.aliases]
rs-example = "rust"
```

Theme bases are `dark` and `light`. Colors accept `#RRGGBB` or the ANSI names
`black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, and `white`, each
with an optional `bright-` prefix. Category names are `comment`, `string`,
`number`, `keyword`, `type`, `property`, `markup`, `function`, `constant`,
`operator`, `punctuation`, `variable`, `tag`, `attribute`, `code`,
`heading_1` through `heading_6`, `list_marker`, `ordered_list_marker`,
`quote_marker`, `code_delimiter`, `fence_language`, `emphasis`,
`strong_emphasis`, `strikethrough`, `link`, `link_target`, `task_checked`,
`task_unchecked`, `table_header`, and `separator`.

Markdown headings have a different color for each level, including headings
underlined with `=` or `-`. Bullets, numbered markers, task boxes, and quote marks
remain distinct in nested lists and quotes. Ordinary prose keeps its terminal
color. Inline backticks and fence delimiters are distinct from code text.
Fences with a recognized language use that language's syntax colors. Markdown
also distinguishes emphasis, strong emphasis, strikethrough, link labels and
targets, table headings and separators, and raw HTML. Reference links and
images use link colors when the current Markdown document defines their label.

Custom themes can set each category directly. An explicit `markup` override
also supplies heading, emphasis, link, task box, and table heading colors that
have no override of their own. Ordered lists inherit an explicitly configured
`list_marker` when `ordered_list_marker` is absent. Explicit `punctuation`
supplies unspecified list markers, quote markers, code delimiters, and
separators. Explicit `code` supplies an unspecified `fence_language`.

Language IDs are `markdown`, `toml`, `yaml`, `json`, `javascript`, `typescript`,
`python`, `rust`, `ini`, `xml`, `html`, and `css`. Association targets can also
be `plain`. Exact basenames take precedence over custom extensions and bundled
extensions. Extensions and fence aliases fold ASCII case. The fence aliases
`text` and `plain` always use neutral code styling.

Place custom Tree-sitter captures in `queries/<language-id>/highlights.scm`.
Use `queries/markdown/inline-highlights.scm` for Markdown inline syntax. For
example, `((primitive_type) @keyword (#eq? @keyword "u32"))` in the Rust query
recolors `u32` using the theme's keyword color. Supported predicates include
`#eq?`, `#not-eq?`, `#match?`, `#not-match?`, their `any-` variants, `#any-of?`,
and `#not-any-of?`. Use the category names above in custom captures. Later
matching custom patterns win. Built-in rules cover the rest of the syntax.

Use the [bundled query registry](src/syntax/engine.rs),
[ede query sources](src/syntax/queries), and grammar versions in
[`Cargo.toml`](Cargo.toml) as starting points. Builds require a C compiler for the
bundled generated grammars. The executable highlights offline.

## Cmd delivery

On a terminal that reports enhanced keyboard support, ede requests disambiguated
key events and accepts Cmd as Super. Detection does not block startup; unanswered
or late capability replies retain portable operation. Acquired keyboard/mouse
modes are restored on exit and suspension.

A terminal can still consume individual Cmd shortcuts. Use the portable
shortcuts in the controls table when Cmd keys are intercepted. A terminal
mapping that sends byte `0x13` for Cmd+S invokes Ctrl+S (Save) in ede. Verify key
mappings in the terminal profile and connection you use, including any tmux or
SSH session.

To try this mapping in iTerm2, add Cmd+S under **Settings → Profiles → Keys →
Key Mappings**, with **Send Hex Code** set to `0x13`. See the
[iTerm2 key mapping documentation](https://iterm2.com/documentation-preferences-profiles-keys.html).

## Storage and architecture

| Module | Responsibility |
| --- | --- |
| `src/bin/ede.rs` / `lib` | CLI, lifecycle boundary, deferred diagnostics |
| `terminal` | Terminal modes, signals, suspend/resume |
| `ingress` / `spool` | Sole stdin reader, bounded VT parsing, streamed paste files |
| `runtime` | Frame scheduling, worker polling, effects |
| `input` / `command` | Text/modifier normalization and shared bindings |
| `app` / `ui` | Modal routing, focus, controls, prepared-frame rendering/hits |
| `picker` | Bounded native directory pages and cancellable background I/O |
| `editor` / `text` | Ordered storage coordination, clipboard/history positions, chunked Unicode layout |
| `document` | Paged text, immutable snapshots, edits, undo/redo, guarded saves |
| `recovery` | Owner locks, immutable scratch objects, atomic catalog, bounded persistence worker |
| `status` | Bounded notifications |

`ede::document::Document` is a blocking storage API. Interactive clients use
`StorageService`: bounded submit/poll/cancel operations, a document owner,
separate workers for immutable reads and file preparation, and at most one
independent save worker. Prepared files are installed and saves reconcile on
the owner before acknowledgement. Public snapshot leases block compaction;
internal background readers pin older immutable generations and allow foreground
compaction. Refresh builds a complete private generation and maps its view before
a short guarded owner swap. Terminal restoration precedes worker shutdown waits.
SIGINT/SIGTERM/SIGHUP use best-effort cleanup and exit `128 + signal`; abnormal
termination retains the last complete scratch checkpoint.

Ratatui/Crossterm provide rendering and mode commands. A dedicated bounded VT
reader avoids Crossterm's whole-string paste allocation. The Unicode segmentation
and width dependencies match Ratatui's resolved versions. Temporary files are
private and removed when their owners exit. The design uses deliberate disk
storage rather than depending on OS swap.

## Development and benchmarks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo bench --locked --bench storage_bench -- ordinary 1024 copy
EDE_BENCH_MIB=1024 EDE_BENCH_CORPUS=ordinary cargo test --release --locked \
  --test terminal large_file_interactive_benchmark -- --ignored --exact --nocapture
EDE_PASTE_MIB=64 cargo test --release --locked --test terminal \
  large_paste_memory_benchmark -- --ignored --exact --nocapture
EDE_RESOURCE_MIB=256 cargo test --release --locked --lib \
  interactive_resource_and_incorporation_measurements -- --ignored --nocapture
EDE_BENCH_MIB=1024 EDE_PASTE_MIB=64 cargo test --release --locked --test terminal \
  large_recovery_and_refresh_benchmark -- --ignored --exact --nocapture
EDE_BENCH_MIB=1024 cargo test --release --locked --lib \
  document::refresh::tests::large_refresh_storage_measurements -- --ignored --exact --nocapture
EDE_BENCH_MIB=1024 EDE_BENCH_CORPUS=ordinary EDE_BENCH_PEAK_RSS=1 \
  cargo test --release --locked --test terminal \
  large_search_and_replace_benchmark -- --ignored --exact --nocapture
cargo test --locked --test terminal isolated_tmux_editing_and_suspend_smoke \
  -- --ignored --exact --nocapture
EDE_BENCH_CORPUS=ordinary cargo test --release --locked --test terminal \
  sustained_piped_capture_benchmark -- --ignored --exact --nocapture
```

The opt-in interactive benchmark generates an allocated file, opens it in a real
PTY, measures first displayed text separately from editable Ready, samples
ordinary event-to-visible latency and editor RSS, navigates/edits beginning,
middle and end, and checks saving during edits. Corpus choices are `ordinary`,
`dense`, and `long`; sizes are MiB. Fixtures are isolated and removed afterwards.
Generated files are warm, and a PTY observer does not measure a graphical terminal
or remote transport. Linux CI exercises storage, editing, scratch recovery, and
refresh and search/replacement with 256 MiB and swap disabled. The search benchmark
counts and replaces one marker per MiB, measures input during a dense-match scan,
cancels, verifies the full saved output, and exercises Undo. Set
`EDE_BENCH_MARKER_STRIDE_MIB=8` for an 8 MiB interval between matches when
testing larger files within the existing undo budget.

The resource probe runs the coordinator and UI against ordinary, dense, long-line
and mixed Unicode files. It records managed allocation high waters, working-file
sizes/allocated blocks, and reducer/update/render CPU time separately from terminal
transport. These counters do not represent total process memory or physical disk
ownership of shared filesystem-clone blocks.
For the executable benchmarks, `EDE_BENCH_PEAK_RSS=1` also reports the kernel's
lifetime peak RSS using `/usr/bin/time` (`-l` on macOS, `-v` on Linux), separately
from live samples. That tool is needed only for this opt-in measurement.

The storage benchmark also accepts `clone` with a copy fallback and optional
churn edits (up to 1000).
