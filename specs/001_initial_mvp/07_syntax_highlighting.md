# 07 — Syntax highlighting

Status — **Implemented, independently reviewed, and validated on the stage branch.**

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
The user authorized implementation and chose Tree-sitter with the pinned
grammars below. The approved Markdown dialect is CommonMark with GFM tables,
task lists and strikethrough. The approved INI dialect uses column-one comments
and treats text after the equals sign as the setting value. No language or
embedding deferral has been approved.

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
- **Highlighting rules.** An optional
  `queries/<language-id>/highlights.scm` supplies capture patterns for a
  bundled grammar's syntax tree. The separate Markdown inline grammar uses
  `queries/markdown/inline-highlights.scm`. On overlapping captures in the same syntax
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

Tree-sitter uses one background analysis helper, isolated
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

Budgets allow up to 64 KiB per input window, 1 MiB total incremental editor-side
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

Tree-sitter is the approved engine. It supports included ranges for embedded
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

Syntect was the alternative. Sublime syntax definitions and resumable line state
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

**Stage 07 acceptance validation complete on the stage branch.** The design
and standalone probe are recorded first, followed by the integrated checks.
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

### First implementation checks on 2026-11-14

Native macOS 27.2 arm64, MacBookPro18,2 with 32 GiB RAM and local APFS,
Rust 1.99.1 and Apple Clang 21.0.0. H03 reducer checks cover native filename
bytes, final extensions, unknown and unnamed tabs, and the full chooser at
40×8 with a click, a hidden dialog, Escape and restored focus. The PTY check
verifies the symlink's opened name, failed and committed Save As, an immediate
Tab Details read after applying a manual choice, and independent choices on
two tabs. Physical terminals and Linux were not exercised in this slice.

Formatting and strict Clippy passed for both crates. The locked workspace
suite passed 230 tests with seven opt-in checks ignored. Independent review
against `745c9ff` caught stale Tab Details after an asynchronous choice and
focus lost through F2 dispatch. Both paths were fixed and re-reviewed against
the focused reducer and PTY checks.

The separately locked [Rust probe](../../examples/syntax_probe/Cargo.toml)
builds published generated grammars with a C compiler, using this inventory.
The probe uses Tree-sitter `0.27.2` (MIT). The listed grammars declare MIT
except INI, which declares Apache-2.0. The release uses the assets at the pinned
crate revisions and includes the [Stage 7 dependency notices](../../licenses/stage07).

| Grammar crate | Version | Grammar ABI |
| --- | --- | --- |
| `tree-sitter-md` | `0.5.4` | 15 block / 15 inline |
| `tree-sitter-toml-ng` | `0.7.0` | 14 |
| `tree-sitter-yaml` | `0.7.2` | 14 |
| `tree-sitter-json` | `0.24.8` | 14 |
| `tree-sitter-javascript` | `0.25.0` | 15 |
| `tree-sitter-typescript` | `0.23.2` | 14 |
| `tree-sitter-python` | `0.25.0` | 15 |
| `tree-sitter-rust` | `0.24.2` | 15 |
| `tree-sitter-ini` | `1.4.0` | 15 |
| `tree-sitter-xml` | `0.7.0` | 14 |
| `tree-sitter-html` | `0.23.2` | 14 |
| `tree-sitter-css` | `0.25.0` | 15 |

`cargo test --offline --locked --manifest-path examples/syntax_probe/Cargo.toml`
passed five tests. The fixtures demonstrate representative upstream captures
for all eleven languages, Markdown block/inline extensions, CRLF YAML
frontmatter, and nested Markdown → HTML → JavaScript/CSS captures at original
byte offsets. They also exercise custom capture priority, rejected invalid
query source and regex syntax, and callbacks during root parsing,
included-range parsing, and a query that yields no matches. These are partial
H01, H02, H04 and H09 evidence. The published Markdown highlighting queries
need added patterns for task markers, table cells and strikethrough. The INI
grammar accepts indented comments and expects LF or CRLF after settings and
comment lines, so the column-one and EOF rules need adapter coverage.

`cargo build --release --offline --locked --manifest-path examples/syntax_probe/Cargo.toml`
built from an empty probe target with a populated Cargo cache in about 15 s.
The runnable probe, including fixture and measurement code, is about 6.0 MiB
stripped. The Stage 06 baseline executable with the Stage 07 Cargo profile
is about 2.6 MiB. The size delta of an integrated editor remains unmeasured.
Each row ran in its own process with `/usr/bin/time -l` and the named argument
to `examples/syntax_probe/target/release/ede-syntax-probe`. The timer covers
root parse and query iteration. Corpus preparation and query compilation take
place before that timer. These runs used warm binary/cache state.

| Probe argument | Input bytes | Parse | Parse and query | Peak RSS |
| --- | ---: | ---: | ---: | ---: |
| `ordinary` (Rust functions) | 7,168 | 1.10 ms | 1.89 ms | 6.9 MiB |
| `near-limit` (Rust comments, 512 lines of 40 KiB) | 20,971,520 | 98.31 ms | 98.47 ms | 27.0 MiB |
| `dense` (JSON arrays) | 4,200,003 | 250.01 ms, cancelled | 250.01 ms | 151.8 MiB |

The dense run demonstrates that a deadline alone does not enforce the helper
memory budget. Inspection of the published C sources found direct libc
allocations in the Markdown and Python scanners, which bypass Tree-sitter's
allocator hook. This baseline was measured before linking the editor's capped
worker. The subsequent implementation and validation are recorded below.

### Integrated implementation checks on 2026-11-15

The editor build uses the selected grammars above, `tree-sitter` `0.27.2`,
`toml` `1.2.1+spec-1.1.0`, and `rlsf` `0.2.3`. The private helper receives an
immutable UTF-8 snapshot through pipes. Rust allocations and the linked
Tree-sitter and scanner allocator entry points share one 64 MiB anonymous
mapping. Darwin uses linker aliases and Linux uses GNU-compatible link wraps.
The editor runs on the system allocator. An independent Darwin disassembly
check verified that the Markdown and Python scanners call the arena aliases.

The editor-side syntax allowance includes 396 KiB of working reservation,
up to two 194 KiB published copies, and a 224 KiB plus 256-byte reservation
for the 48 KiB transfer window with the default storage page size. Snapshot
leases use the existing background storage admission. The receiver checks the
projection nonce, syntax category, glyph boundaries, and complete mapped
coverage before publishing. It shrinks the working reservation to the retained
range allocation and releases the tab's projection when parking the tab.

- H01–H02 — Installed helper fixtures cover every grammar, Markdown inline
  and block syntax, unclosed fences, the strict YAML prefix, quoted/indented
  embeddings with disjoint offsets, eligible HTML types, and neutral unknown
  types. Runtime assertions check the worker's allocation routing counters.
- H03/H06 — PTY checks verify source coloring, per-tab language choice across
  Save As and switches, syntax foreground under selection and current search
  matches, wide and combining text, and fallback with editable/searchable
  text. The layout-floor test exercises keyboard and mouse at 40×8.
- H04/H05 — Tests cover CRLF across transfer windows, standalone CR, byte
  limits, offscreen line fallback and refresh recovery, invalid helper offsets
  and IDs, stale viewport failure, and a real uncooperative child killed and
  reaped at deadline and on suspension. Sixty-four highlighted tabs retain
  an available foreground edit/save budget. Isolated mutations reproduced
  the stale viewport fallback and parked-tab projection leak. An embedded
  host PTY mutation reproduced re-execution of the host when the helper was
  incorrectly inferred from the process executable.
- H07 — A synthetic retained read-only tab remains searchable, projects its
  manual Rust choice from an immutable snapshot, rejects the earlier viewport
  after retained-base eviction, and keeps its choice through editable EOF.
  Real pipe ingestion follows in Stage 8.
- H09 — PTY checks exercise a custom palette, association, query, palette-only
  repaint, failed reload, and removal of overrides. A gated loader test proves
  a superseded revision cannot publish. Helper tests cover the Markdown inline
  query, later user capture priority and valid/invalid query source.

On native macOS 27.2 arm64 (MacBookPro18,2, 32 GiB RAM, local APFS), the
stripped release executable is 8,113,408 bytes. The release helper heap-ceiling
test `oversized_projection_reaches_fixed_heap_ceiling`, timed with
`/usr/bin/time -l`, exited with the budget code and 48,807,936 bytes peak RSS.
The final H08 release PTY `large_highlighting_and_search_overlap` used a warm
20,889,622-byte Rust document and an allocated 1 GiB file in an inactive tab.
It observed syntax worker CPU while search scanned the 1 GiB tab. First text
took 259.4 ms. Eight ordinary input samples had a 21.2 ms maximum. Sampled
RSS during the scan was 7,056 KiB for the editor and 26,896 KiB for its worker.
The editor command's kernel lifetime peak was 26,896 KiB. The metrics cover
the local PTY and its prepared files. Native Linux CI exercises the same
overlap under a 256 MiB no-swap cgroup. The first Linux attempt was killed at
the cgroup limit while using a fixture generator that synced only at EOF.
The benchmark now syncs and drops prepared pages in intervals on Linux and
reports when fixture preparation finishes.

A direct release worker run on a comparable 20,889,615-byte Rust corpus
received and checked the UTF-8 payload over pipes in 8.83 ms. Parse and first
projection took 112.36 ms. A later projection reused the trees and took
0.06 ms. Peak charged arena use was 23,130,501 bytes. The direct run excludes
storage snapshot I/O and editor-side preflight. The rebuilt binary also
emitted Markdown heading color for `FUTURE.md` in an isolated PTY and quit
cleanly.

The integrated local workspace run passed 248 tests with nine opt-in checks
ignored. The capped helper and 1 GiB overlap release probes passed separately
on macOS. Formatting and Clippy passed with warnings denied. Independent
engine and runtime re-reviews found no remaining production blockers after
the fixes. Physical terminals, tmux and remote transports were outside this
validation run.

Linux's unoptimized debug build timed out on the combined nested-language
fixture. The development profile optimizes the Tree-sitter runtime, regular
expression dependencies and fixed-arena allocator. The runtime deadlines
and release profile are identical to the approved implementation.

The [full CI matrix](https://github.com/rapidclock/ede/actions/runs/43707728892)
passed for code revision `50691b2` on the hosted macOS and Ubuntu runners
using Rust 1.99.1. The Linux release helper exited with the expected budget
code and 49,344 KiB peak RSS. The Linux overlap ran in a cgroup with
`MemoryMax=256M` and `MemorySwapMax=0`. First text took 24.1 ms and the slowest
of eight input samples took 16.8 ms. Sampled RSS was 7,888 KiB for the editor
and 28,540 KiB for its worker. systemd reported a 256 MiB cgroup peak with
zero swap. The same run passed the existing Linux constrained storage,
editing, recovery, refresh, and search and replacement regressions.

The earlier local run inherited a high open-file limit. At a soft limit of
256, the 64-tab search test reproduced `EMFILE` while opening tabs on both
Stage 06 (`9714b09`) and Stage 07 (`c807466`). The 64-tab syntax PTY also
reproduced the error while opening its 62nd file before the fix. Each tab
keeps four private storage files open, and the editor needs additional
descriptors for snapshots and background I/O. The CLI and editor unit-test
fixture now attempt to raise their process soft limit to 1,024, capped by the
inherited hard limit. Embedded sessions keep their caller's limits.

On macOS, plain `cargo test` from a soft limit of 256 passed 248 tests with
nine opt-in checks ignored, including 64 tabs with search and cancellation,
and the 64-tab PTY with highlighting, editing, saving and quitting. The PTY
checks the committed file before quitting and also passed in release mode
with timing enabled. CI starts the regular workspace test step with a soft
limit of 256 on both hosted platforms. Independent review against `c807466`
confirmed the soft limit adjustment preserves the inherited hard limit and
passed both focused 64-tab tests under the same macOS limit. The two focused
tests also passed locally with a hard limit of 512. The full workspace
suite passed in release mode. The [follow-up CI matrix](https://github.com/rapidclock/ede/actions/runs/43755157165)
passed for `9802a1c`, including the low-limit workspace tests on both hosts
and all Linux constrained-memory checks.
