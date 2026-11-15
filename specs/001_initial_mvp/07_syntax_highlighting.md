# 07 — Syntax highlighting

Status — **Design draft. Engine selection and implementation authorization pending.**

Branch — `stage/syntax_highlighting`, based on `feat/initial_mvp` at `9714b09`.
PR target — `feat/initial_mvp`.
Requirements — [FR-04](overview.md#fr-04--syntax-highlighting-and-language-expansion),
with [Stage 02 storage](02_document_storage.md), [Stage 03 terminal rendering](03_editing_and_mouse.md),
[Stage 04 tabs](04_tabs_and_file_picker.md), [Stage 05 refresh](05_scratch_recovery.md),
and [Stage 06 search](06_search_and_replace.md) as integration baselines.

## Outcome and decision status

Users see syntax colors while reading and editing supported UTF-8 text. They
can choose a language or Plain Text for a tab and use built-in or custom syntax
colors. They can customize file associations and the highlighting rules of
bundled grammars through local files. Highlighting works offline. Document
text appears as soon as the existing editor can display it. Editing, selection,
navigation, search, and saving remain usable during highlighting and after any
highlighting failure.

Required scope includes all eleven languages and the embedded cases below.
The 20 MiB, 64 KiB, and 250 ms thresholds are already agreed starting defaults.
The detection rules, customization format, interaction, palettes, dialect
boundaries, worker design, and additional resource caps below form the proposal.
Confirm the engine, grammar sources, and Markdown/INI dialects with the user
before adding dependencies. No supported language or required embedding is deferred.

## Detection and language coverage

In Auto, match the tab's current stored filename using the precedence in
[User customization](#user-customization). Built-in detection matches the
final extension ASCII case-insensitively. Match native filename bytes so a
non-UTF-8 stem works.
Use the name the editor opened or committed with Save As, including for files
opened through a symlink. An unnamed scratch starts in Plain Text, and named
tabs with no matching association use Plain Text. Ordinary edits and snapshot
refreshes do not run content sniffing or change the language.
Re-evaluate Auto after a successful Save As, using its committed destination
even if newer edits keep the tab dirty. A cancelled or failed Save As leaves
the previous selection and detection intact.

| Language | Filename suffixes | Fence identifiers beyond the canonical language name | Fixture coverage |
| --- | --- | --- | --- |
| Markdown | `.md`, `.markdown` | `md` | Block/inline constructs, extensions and embeddings below |
| TOML | `.toml` | | Tables, dotted keys, strings, numbers, arrays and comments |
| YAML | `.yaml`, `.yml` | `yml` | Keys, sequences, scalars, block strings, anchors, aliases and comments |
| JSON | `.json` | | Properties, strings and escapes, numbers, booleans, null and delimiters |
| JavaScript | `.js`, `.mjs`, `.cjs` | `js`, `mjs`, `cjs` | Keywords, declarations, comments, strings and template substitutions |
| TypeScript | `.ts`, `.mts`, `.cts` | `ts`, `mts`, `cts` | JavaScript constructs plus type annotations, interfaces and generics |
| Python | `.py`, `.pyi` | `py`, `pyi` | Keywords, comments, definitions, strings and f-string substitutions |
| Rust | `.rs` | `rs` | Keywords, items, types, macros, comments and raw strings |
| INI | `.ini` | | Section headers, `key=value`, empty values, and column-one `#` / `;` comments |
| XML | `.xml` | | Tags, attributes, comments, entities and CDATA |
| HTML | `.html`, `.htm` | `htm` | Tags, attributes, comments and embedded scripts/styles |
| CSS | `.css` | | Selectors, properties, values, at-rules and comments |

Markdown uses CommonMark source constructs plus GitHub-style pipe tables, task
markers and strikethrough. Highlight headings, lists, quotes, emphasis, links,
inline code, and fence delimiters. Recognize YAML frontmatter bounded by `---`
on the first logical line and a later standalone `---` line, accepting LF and
CRLF. An unmatched opener follows ordinary Markdown rules. In backtick and
tilde fences, take the first whitespace-delimited word of the info string and
resolve it ASCII case-insensitively through the registry. An unknown, absent,
or `text`/`plain` identifier gives the body a neutral code style. Use the
Markdown grammar's block and inline context so markers inside code are treated
as source text for that code. An unclosed fence extends to EOF under ordinary
Markdown fence rules.

In HTML, parse bodies of `<script>` and `<style>` as JavaScript or CSS when
`type` is omitted, empty, or recognized. Trim HTML whitespace and compare type
values ASCII case-insensitively. Accept `module`, `text/javascript`, and
`application/javascript` for scripts and `text/css` for styles. Keep unknown
types neutral, including JSON data scripts. Apply the same HTML rules inside
an HTML fence. Keep injection ranges inside their parent, and map every nested
capture back to original logical bytes, including indented fences. Place
frontmatter, fence alias, and HTML type rules with syntax definitions.
Treat INI values as text through the line end. A comment marker inside a value
stays part of the value. Incomplete or malformed source still receives any
unambiguous styling the grammar can recover, with unmatched text in the
default style.

## Commands and rendering

- Add named `Language…`, `Syntax Theme…`, `Reload Highlighting`, and
  `Retry Highlighting` actions to the existing F2 Actions menu and help as
  appropriate. Reach the menu and picker rows by keyboard and mouse. Use the
  existing dialog focus and
  scrolling conventions. Up/Down moves, Enter or a row click applies, and
  Escape dismisses without changes and returns focus. Keep all options
  reachable at 40×8; at a size too small to show the dialog, preserve the
  pending choice and allow Escape or resize. Additional default shortcuts
  belong to the Stage 09 keymap.
- Language targets the tab captured when the picker opens. Show Auto and its
  detected language, Plain Text, and each bundled language, with the current
  choice marked. Apply only if that tab still exists. A manual selection
  follows the tab across switching, refresh, and Save As. Its lifetime is
  the open tab. Scratch recovery continues to persist text through Stage 05.
  Applying a language or theme does not move the caret/selection, mark the
  tab modified, change its undo history, or write a file.
- Syntax Theme is session-wide. Offer built-in Dark and Light palettes using
  ANSI colors for a portable baseline, and list valid user themes alongside
  them. Start with the configured default or Dark. Map semantic categories
  such as comment, string, number, keyword, type, property and markup to
  foreground colors. Theme changes restyle current spans without parsing.
  Preserve terminal-default colors for ordinary text and leave editor chrome
  and terminal background to existing settings. Stage 09 supplies preference
  persistence and configuration of the highlighting thresholds.
- Show the effective language and highlighting state in Tab Details. Keep a
  visible status indication when highlighting falls back, with the full reason
  in Tab Details if the status row truncates. Use distinct states for intentional
  Plain Text, preparing, highlighted, and fallback. Syntax status must coexist
  with active search, dirty state, recovery and on-disk change warnings.
- Use the existing visible byte-to-cell map. Syntax provides foreground only.
  Apply ordinary selection over syntax, ordinary search matches over selection,
  and the current match over ordinary search matches. Keep cursor visibility,
  markers, and hit testing unchanged. Use the category covering the first
  scalar of an ordinary grapheme for its entire cell span; apply the same
  rule across tabs, wide glyphs and clipped/escaped glyphs. Never split a
  grapheme or feed raw document control bytes to the terminal. Search retains
  its exact logical ranges and existing intersecting-cell behavior.

Stage 08 supplies stream ingestion. The Stage 07 interface accepts an immutable
snapshot of retained text independent of editability. A stream starts in Auto
with no filename. Its manual override remains on the same tab when EOF makes it
editable. Appends, truncations, retained-base changes and EOF conversion must
provide a fresh revision or document identity whenever the logical coordinates
or text change. Verify that contract with synthetic revisions in this stage and
validate real ingestion in Stage 08.

## User customization

Proposed location on macOS and Linux is the `ede` directory under an absolute
`XDG_CONFIG_HOME`, or `~/.config/ede` when that variable is unset or invalid.
Load optional `highlighting.toml` and query files there. Missing files use
bundled defaults. Load on startup and when `Reload Highlighting` is invoked.
The editor owns this interface and documents the effective location, theme
keys, language IDs and how to obtain the bundled queries as starting points.

- **Colors.** In `highlighting.toml`, `default_theme` selects a built-in or
  user theme. Each `[themes.<name>]` entry sets `base` to `dark` or `light`
  and supplies any overrides in `[themes.<name>.colors]`. Accept ANSI color
  names and `#RRGGBB` foreground values for documented capture categories.
  Unspecified categories inherit the base. Reject duplicate or reserved theme
  names and unknown categories. The F2 choice lasts for the session and the
  file supplies the persistent default. On reload, retain an explicit session
  choice that still exists. Otherwise use the file's default or Dark.
- **Associations.** `[files.names]` maps exact, case-sensitive basenames to a
  bundled language ID or `plain`. `[files.extensions]` maps final extensions
  without dots, matching ASCII case-insensitively. Auto uses exact basename,
  custom extension, built-in extension, then Plain Text in that order. Manual
  language choices retain priority. `[fences.aliases]` adds or overrides
  Markdown fence aliases for bundled languages or `plain`; it takes priority
  over bundled names and aliases. Reserve `text` and `plain` after ASCII
  case folding.
  Reject extensions or fence aliases that duplicate another user entry after
  ASCII case folding.
- **Highlighting rules.** For the proposed Tree-sitter engine, an optional
  `queries/<language-id>/highlights.scm` supplies capture patterns for a
  bundled grammar's syntax tree. On overlapping captures in the same syntax
  tree, matching user patterns take precedence over built-in patterns and the
  later matching user pattern wins. Bundled rules still style other text and
  nested language bodies retain their own rules. Document the supported query
  predicates and category names. A query changes the styling of parsed syntax.
  Runtime loading of new parser grammars remains an open design decision.

Load and validate configuration off the interactive path. Cap combined
configuration and query source at 256 KiB. Compile candidate queries within
the analysis worker's deadline and memory limits. Coalesce startup and reload
requests into one running validation and one latest pending request. Publish
only the latest requested revision after every file validates. On failure,
report the failing file and reason and retain the last good revision, which
starts at the bundled defaults.
Removing overrides and reloading restores the corresponding defaults. A palette
change alone repaints without parsing and leaves fallback state intact. Any
query or fence alias change advances the analysis configuration generation,
revokes current and in-flight syntax results, and allows a fresh attempt on
the next eligible view. This covers aliases that previously styled a fence as
neutral and queries used only in embedded text. Re-evaluate file associations
on Auto tabs, including parked tabs, and invalidate analysis where the effective
language changed. Keep document text, dirty state and history intact through
reloads.

## Eligibility and fallback

| Condition | Required behavior |
| --- | --- |
| Logical length exceeds 20 MiB (20,971,520 bytes) | Reject highlighting before reading or parsing text. Count UTF-8 bytes excluding the preserved BOM. |
| Any logical line exceeds 64 KiB (65,536 bytes) | Fall back for the entire revision, even if the long line is offscreen. Count bytes excluding LF/CRLF terminators; standalone CR counts as text. Handle window-split CRLF, EOF without a newline, and a trailing standalone CR. |
| A parser operation exceeds 250 ms | Cancel it and fall back for the whole document. The deadline covers the root parse, all embedded parses, queries, and span assembly for the first projection as one operation. Later projections each have a 250 ms deadline. Measure monotonic time from dispatch to the ready analysis worker, excluding preparation reads and queueing. Discard late completions. |
| Parser/read failure, invalid worker output, or analysis budget exhausted | Report a short reason and show the entire document in plain text. A bad grammar or query must not terminate the editor or disable unrelated languages. |

Size and line limits admit equality. Check eligibility on the exact immutable
revision; publish syntax only after successful whole-document preflight and
analysis. If a later projection fails, clear current coloring and record the
fallback. Plain Text leaves syntax work idle. Display first text and changed
views immediately in plain text whenever matching syntax results are pending.

Recheck size or line fallback after a new text revision (including Undo or
refresh), an applicable limit change, or explicit Retry. Latch a timeout,
engine/read error or budget failure for the tab and effective language across
edits, scrolling, switches, and theme changes. A deliberate Retry, change to a
different language, or applicable configuration change may start another
attempt. Retry applies only to the captured live tab and never bypasses limits.
Superseded work is cancellation rather than a failure and must not latch a
fallback on the replacement request. Shared storage admission pressure may
defer an attempt; retry only when capacity becomes available or on explicit
Retry, allowing foreground work first.

## Coordination and resource policy

Keep detection, grammar and query registration, embedding rules, semantic
categories, and theme mapping independent of the TUI. A new language registers
its names, parser and queries plus fixtures without changing the picker,
renderer, terminal input handling, or document store. Registrations and
assets ship with the executable.

The coordinator publishes separate syntax status and optional, immutable
viewport spans alongside the existing screen. Use a key containing tab ID,
document ID, logical revision, effective language, and an attempt generation.
Include the applicable query and embedding-rule revision in that identity.
Projection also carries the exact viewport identity (geometry, scroll origin
and generation). Accept a result only if its identities still match the
current tab, document metadata and prepared view. Project using the visible
grapheme boundaries from the existing byte-to-cell map. Send bounded logical
byte descriptors of visible glyphs and strided runs; include the full bounds
of a partially clipped glyph. The helper resolves the category at each first
scalar and returns ordered, nonoverlapping runs tagged with the viewport
identity. Validate the categories and each run's endpoints and coverage
against the original descriptors before publication. Reject ranges that split
a glyph or bridge unmapped text, even if their endpoints lie within the
document. Release any view reference used to prepare the descriptors before
waiting on analysis. Changes to text, effective language, or lifecycle revoke
former analysis; changes to the view revoke only its projection. Reconcile
inactive refresh through the ordinary tab state and start fresh when activated.

The proposed Tree-sitter path uses one background analysis helper, isolated
from terminal I/O and storage ownership. Reuse the current executable in a
private worker mode with a bounded protocol over dedicated pipes. The helper
never acquires the terminal or reads the editor's document stdin and receives
no document paths. Check length before requesting text and check line lengths
while transferring the logical bytes from one immutable snapshot. The helper
starts parsing only after preflight succeeds and the transfer is complete.
Submit the existing non-save-blocking `BackgroundSnapshot` request
asynchronously. A job holds at most one lease during transfer, releases it on
completion or cancellation, and can pin an old generation without blocking
compaction publication. Highlighting has no write capability. Keep at most one
running job and one replaceable latest request, with no snapshots queued, no
worker per tab and no syntax trees retained for parked tabs. Coalesce rapid
edits and view changes. Keep at most one active revision's text and parse data
for its later projections; fresh parses after edits are acceptable if the
responsiveness checks pass. Keep reads, parsing, IPC, validation and cleanup
off the terminal and storage owner loops. Foreground commands never wait for
optional analysis to finish.

Proposed budgets are 64 KiB per input window, 1 MiB total incremental editor-side
buffer/IPC/result allocation charged to the existing 32 MiB shared buffer
pool, and 64 MiB of helper dynamic allocation including input, query state,
trees, nested parses, and staging. Cap emitted projections at 16,384 normalized
ranges, accounting for active, pending and retired copies in the editor-side
budget. Coalesce adjacent equivalent ranges and fall back if a complete
projection would exceed its cap. Record charged peaks and process RSS
separately. Prove a real helper allocation ceiling on both platforms,
including grammar scanner allocations. A budget estimate from input bytes
alone is insufficient. Validate the combined process footprint alongside
existing large-file workloads.

Use cooperative deadline/cancellation checks in parsing and queries. If a
native call stops cooperating, a supervisor terminates and reaps the helper
using the same deadline. Bound helper startup and pipe waits separately.
Closing a tab, switching away, changing language and quitting cancel obsolete
analysis. Reap an aborted helper before replacing it; a new eligible request
can start a fresh helper without re-enabling a latched failed request. Ensure
accepted Quit and error exit restore terminal state independently of helper
cleanup and leave no child, snapshot, or pipe behind. Cancelled Quit preserves
the editor session. On the editor's supported Ctrl+Z / SIGTSTP suspend path,
stop admission, revoke active analysis and terminate the helper before stopping
the editor. Resume with fresh analysis for the eligible active viewport once
terminal modes are restored. Treat this as cancellation and preserve existing
fallback latches.

## Engine decision and feasibility

Tree-sitter is the leading proposal. It supports included ranges for embedded
languages and programmable capture queries, and its Rust bindings expose
parse/query progress callbacks. The Markdown candidate provides block and
inline grammars and relevant extensions. Native code and per-language
grammars raise build, memory and maintenance costs. The upstream C runtime
aborts on allocator failure, which motivates the proposed containment.
[Highlight queries and injections](https://tree-sitter.github.io/tree-sitter/3-syntax-highlighting.html)
· [Rust bindings](https://github.com/tree-sitter/tree-sitter/blob/master/lib/binding_rust/lib.rs)
· [Markdown grammar](https://github.com/tree-sitter-grammars/tree-sitter-markdown)
· [Allocator behavior](https://github.com/tree-sitter/tree-sitter/blob/master/lib/src/alloc.c)

The editor controls the theme and custom file loader under either approach.
Tree-sitter queries can style the syntax nodes exposed by the chosen grammar.
Feasibility must exercise user-authored queries on bundled grammars, including
invalid rules and costly matches, under the same analysis limits.
[Tree-sitter query syntax](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html)

Syntect is the alternative. Sublime syntax definitions and resumable line state
offer a smaller integration surface if the bindings cover all required forms.
The pure-Rust regex option avoids Oniguruma; its worst-case interruptibility and
the exact bundled syntax and injection coverage still need proof. Measure it
against the same fixtures and deadlines if Tree-sitter cannot meet them.
[Syntect upstream](https://github.com/trishume/syntect)

Before selecting dependencies, validate the candidate in a Rust feasibility
probe outside the application. Compare coverage and embedded-query accuracy,
cancellation of no-output and nested work, realistic/adversarial resource use,
release binary size/build cost, and pinned-toolchain builds on macOS and Linux.
Inventory exact grammar/query versions, ABI, licenses and notice obligations,
including candidates for [TOML](https://github.com/tree-sitter-grammars/tree-sitter-toml),
[YAML](https://github.com/tree-sitter-grammars/tree-sitter-yaml),
[INI](https://github.com/justinmk/tree-sitter-ini), and
[XML](https://github.com/tree-sitter-grammars/tree-sitter-xml). Prefer released
bindings with bundled generated sources. Document any needed C/C++ compiler;
a normal build should not require grammar generation, Node.js, dynamic grammar
downloads, or a language server. Show the user the evidence and record the
agreed engine and sources here. If a candidate fails a requirement, bring the
specific gap and options back for decision before reducing coverage or
relaxing limits.

## Acceptance criteria and validation plan

| ID | Required observable evidence |
| --- | --- |
| H01 | Fixtures assert semantic categories by logical byte range for each listed language, including representative incomplete text. Adding a registry entry yields detection, chooser and styling without TUI changes. A release build highlights offline on both platforms. |
| H02 | Markdown fixtures cover block/inline, task markers, tables and strikethrough; first-line YAML with LF/CRLF and unterminated/noninitial delimiters; tilde/backtick, indented and unclosed fences with known, unknown and absent identifiers. HTML fixtures assert JavaScript/CSS within HTML, unknown types staying neutral, and embedding within an HTML fence. Check original offsets and multiline context when scrolled. |
| H03 | PTY and layout checks use keyboard/mouse to choose Language and Theme, cancel, resize and reach every option. Cover mixed-case suffixes, non-UTF-8 stems, unknown and unnamed tabs, Save As success/failure, manual override across refresh and switches, explicit Plain Text, and repaint without reparse on theme change. |
| H04 | At sizes just below, at and above both byte limits, verify pre-read size rejection and offscreen long-line fallback. Include multibyte scalars, BOM, CRLF across read windows and trailing CR. Use a controlled clock/worker to force timeout during root parse, nested parse and query, and prove a real noncooperating helper is killed and reaped. Verify whole-document fallback, retained reason, no automatic timeout retry, explicit Retry and re-eligibility after shrinking. |
| H05 | Synchronize old work against edit, Undo/Redo, Replace All, refresh, Save As, language switch, scroll/resize, tab switch, close and Quit. Gate preflight/transfer while verifying that foreground input and save/maintenance publication can proceed. Assert stale output is rejected, cancelled Quit keeps the tab usable, and leases/children are reclaimed. Inject read failure, helper memory exhaustion, helper exit and malformed replies, including a mid-scalar endpoint and a span over unmapped text. Verify fallback and recovery. In a job-control PTY, suspend with an active uncooperative helper, verify analysis has stopped while the editor is suspended, then resume and verify terminal modes and fresh results. |
| H06 | Terminal cell checks verify ANSI palettes, selection and search precedence, wide and combining graphemes, tabs, escaped/clipped glyphs, cursor and hit testing. Include narrow views of long lines, where visible byte runs can be far apart. PTY checks edit, search/replace, Undo and save exact bytes before, during and after fallback. |
| H07 | A synthetic retained read-only stream with manual override remains searchable and highlightable, rejects old offsets after a retained-history change, and keeps the override across the editable EOF transition. Real stream testing follows in Stage 08. |
| H08 | Run a small representative document, near-limit regular and pathological corpora, repeated edits and switches across 64 tabs, and a 1 GiB file that takes the size fallback. In a separate overlap run, verify active helper work on an eligible tab while all-open search scans an inactive 1 GiB tab. Record first visible text, ordinary input under load, preflight/parse/query time, cancellation, both process peaks and combined memory, and budget high waters. Check the 1 s first-text and 50 ms ordinary-input local goals and existing storage limits on the agreed baseline and run the resource case in the swap-disabled Linux 256 MiB harness. |
| H09 | Without rebuilding, load a custom theme, a file association, a fence alias, and a rule that reclassifies a parsed token. Check theme inheritance and startup/reload choices, detection precedence and manual override, and built-in styling outside custom matches. Prove palette-only reload avoids parsing. Gate an old reload against a newer request and old syntax across a query change or a formerly unknown fence alias. Malformed or oversize input and query compilation timeout keep the last good configuration, with bundled defaults on first load and a visible diagnostic. Removing overrides restores defaults. |

Use a failing focused check before each behavior change. Use explicit ordering
gates for H05 and subprocess/PTY tests for real process and terminal behavior.
Run formatting, Clippy with warnings denied, the workspace suite, and relevant
Stage 02–06 regressions. Record OS, toolchain, filesystem, CPU/RAM, corpus,
cache state, build profile, limit enforcement, and reproducible commands for
H08. Report native macOS/Linux and PTY evidence separately from physical
terminals, tmux and remote transports. Map checks and measurements to H01–H09
in the record below.

## Validation record

**Stage 07 acceptance validation pending.** Engine/grammar feasibility,
native parser and terminal behavior, and performance evidence are pending.
No language or embedding deferral has been approved.

Design baseline on 2026-11-14 — merged Stage 06 at `9714b09`, native macOS
27.2 arm64 and Rust 1.99.1. The branch includes the explicit `ede` binary
target and release profile supplied with the design request, and the
`0.0.1-dev.7` manifest/lockfile update. The runtime baseline passed
`cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --offline --locked -- -D warnings`,
and `cargo test --workspace --offline --locked` (227 passed, seven opt-in
checks ignored). `cargo build --release --offline --locked --bin ede`
passed and `target/release/ede --version` printed `ede 0.0.1-dev.7`.
These runs exercise the merged runtime with the updated Cargo settings.

Independent design review followed [REVIEW.md](../../REVIEW.md) against
`9714b09`. Review findings on snapshot admission, validation of worker spans,
job-control suspension, and overlapping highlight/search measurements were
addressed and re-reviewed. No actionable findings remained in the initial draft.

The customization follow-up received a separate design review against
`f968931`. The review verified the final reload ordering, case-insensitive
association precedence, and rejection of stale embedded results. The revision
passed whitespace and all 29 local Markdown link checks; H01–H09 are unique
and sequential. H09 runtime validation remains pending.
