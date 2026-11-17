# 10 — Integrated MVP validation

Status — **Design draft. Integrated validation pending.**

Branch — `stage/mvp_validation`. PR target — `feat/initial_mvp`.

Depends on Stages 01–09, integrated through Stage 09 at `dfacfe2`. The
[approved MVP overview](overview.md) defines the scope and FR-01–FR-10. This
draft specifies the validation scenarios, reference environments, and proposed
pre-release changes. Exact terminal versions and profiles are determined by
the runs. Approval of this draft and authorization to carry it out are
separate decisions.

## Outcome and scope

A source build of ede at package version `0.0.1` can be used for ordinary editing,
occasional editing of files larger than RAM, and live-output inspection. The
stage closes when the acceptance criteria have recorded evidence and a user can
build, operate, configure, and recover the editor with the README. Reuse the
existing CLI, TUI, storage policies, and tests. Implementation work covers
validation tooling, fixes needed to satisfy the approved contracts, and any
pre-release proposals below that are accepted. Packaging, installers,
automatic updates, release distribution, and deployment are outside this stage.

Use prior validation records for detailed behavioral coverage, failure guards,
and known limitations. Match each reused result to the code and environment it
actually exercised. Rerun applicable evidence when intervening changes affect
its behavior or conditions. Run the integrated workflows, candidate build and
native gates on the candidate revision. A Stage 01 or Stage 03 deferral of a
manual check leaves it open for Stage 10.

## Proposed pre-release changes

The user reported that Copy in ede on Ubuntu left the Mac clipboard unchanged,
both through their tmux pane and in Terminal.app over direct SSH. They proposed
a Settings toggle to keep both clipboard workflows. They also reported that
physical Cmd shortcuts invoked Mac or terminal actions, including a dialog to
export terminal output on Cmd+S. The proposed clipboard details and UI scope
below are for design review. The existing Cmd requirement already applies.

### Clipboard setting and client paste

Stage 03 defines a private clipboard per running ede process. Add a proposed
**Copy to system clipboard** toggle to Settings. In `config.toml`, use the
optional boolean `clipboard.copy_to_system` under `[clipboard]`. Omission,
Restore Defaults, and `--safe-config` select `false`. Apply changes through the
existing save/reload revision path and retain the last good revision on an
invalid reload. An existing config needs no migration.

- In either mode, explicit Copy and committed Cut populate ede's private
  clipboard. `Ctrl+V` continues to paste that clipboard. The terminal's Paste
  action supplies system clipboard text through normal terminal input. A
  delivered Cmd+V shortcut continues to invoke ede's Paste command. If the
  terminal handles Cmd+V itself, insert the received text once.
- With the applied setting enabled at command dispatch and at output admission,
  also send the captured text using one OSC 52 clipboard write. Do this only
  after internal Copy succeeds or the Cut deletion commits. The destination is
  the client terminal's system clipboard, including when ede runs remotely.
  Use only the write operation. Reading system clipboard contents is outside
  this integration.
- Propose an 8 KiB limit on the UTF-8 byte count of the outgoing selection.
  Check exact round trips up to the proposed limit on each supported path.
  Leave larger selections available internally and report the reason they
  were kept internal. Send one bounded base64 payload with terminal framing.
  Keep spool reads off the UI thread and serialize the frame with other tty
  output. Never emit raw selection bytes as terminal commands or write the
  sequence to redirected document output.
- No selection, failed Copy, and failed or cancelled Cut send no clipboard
  write. A successful Cut whose external write fails leaves all cut text in
  the internal clipboard. Before transmission, retire a pending write if
  overtaken by a newer successful Copy or Cut, a change back to internal-only,
  or terminal suspension or teardown. Re-enabling starts with future commands
  and cannot revive an older write. Serialize accepted writes so older data
  cannot follow a newer write.
- Finish an admitted frame before sending unrelated output. If the write fails
  after sending any frame bytes, recover the terminal parser with a verified
  abort sequence before drawing again, or treat the tty as unusable under the
  existing terminal-loss policy. Do not retry a partial payload or wait
  indefinitely. Recoverable write failures preserve the internal clipboard and
  committed Cut state. A lost tty follows the existing interrupted-session
  recovery rules.
- Report **Copied internally** or **Cut to internal clipboard** when no write
  is sent. Include the reason when the requested write was skipped or failed.
  When the frame has been sent, report that a system clipboard request was
  sent. A successful tty write cannot confirm the client accepted it.

Document the terminal's clipboard write permission and tmux forwarding
requirements for setups that pass. A tmux copy-mode selection reaching the Mac
clipboard establishes only the tmux-originated path. `set-clipboard external`
can permit that while ignoring writes by applications in a pane. Verify
`set-clipboard on`, the clipboard capability, and the outer terminal's
permission as applicable in a disposable profile. Document the tested
terminal selection or tmux copy-mode fallback where OSC 52 is unavailable.
The editor leaves terminal and tmux settings under the user's control.
[Stage 03 clipboard policy](03_editing_and_mouse.md#mouse-and-clipboard),
[tmux clipboard settings](https://github.com/tmux/tmux/wiki/Clipboard),
[iTerm2 clipboard permissions](https://iterm2.com/documentation-preferences-general.html).

### Empty startup, tabs, and picker

- Show a welcome view if startup resolves with zero tabs. Offer **Open File** and
  **New Scratch** with their effective shortcuts, portable defaults Ctrl+O
  and Ctrl+N, and mouse actions using the existing command routes. Preserve
  startup and recovery order and restore retained scratches as usual. Keep
  each file-open error visible in the resulting session, including when there
  are zero tabs. Startup errors that prevent the interactive session still use
  the existing exit diagnostic. Successful opens, recoveries, pipes, and
  followers use their existing tab flow. A scratch is created by New Scratch
  or an existing explicit workflow such as stdin promotion. The same welcome
  view works after closing the last tab. This supersedes the automatic blank
  scratch in Stages 04–05 if accepted.
- Give each visible tab a compact visual boundary and distinguish the active
  tab through its shape or text emphasis as well as any color. Preserve
  modified and attention indicators, tab hit targets, and access to overflow
  tabs at 40×8. Keep the header to its existing one row.
- In the Open picker, a single click selects a row. Two consecutive completed
  left clicks on the same current row within a proposed 400 ms interval invoke
  the existing Open action once, entering a directory or opening a file. Keep
  keyboard Enter and the Open button. Clear click history on activation, a
  different target, a drag, focus loss, or a listing or layout change, so an
  old click cannot activate another path.

The user clarified that reStructuredText, CSV, and TSV support means syntax
highlighting. Defer those additions until after `0.0.1` under this proposal.

## Candidate and test fixtures

- Build a release-profile binary from the candidate revision with the pinned Rust
  toolchain, a native C compiler for the bundled grammars, and the checked-in
  Cargo lockfile. Use that binary for terminal, manual, and performance runs.
  Where a test builds its own binary, record its profile and revision. Run
  native macOS and Linux checks with their own binaries.
- Start fixture-based sessions with isolated working, config, state and temporary
  directories, an absolute `XDG_CONFIG_HOME` and `EDE_STATE_DIR`, a supported
  locale, and a usable `TERM`. Use isolated tmux sessions and disposable files,
  producer processes, and fault targets. Use a separate profile for tests that
  intentionally exercise default path resolution or settings changes.
- Prepare UTF-8 fixtures with distinctive markers and byte-for-byte expected
  outputs. Include BOM, mixed LF/CRLF and a final unterminated line, tabs,
  combining text, and wide characters. Add distinct invalid UTF-8, recognized
  unsupported encoding, NUL/binary, and directory targets. Provide a Markdown
  fixture with YAML frontmatter and a bundled-language fence, HTML with
  JavaScript/CSS, a fixture for each bundled grammar, and an unknown fence.
- Use real child processes, PTYs, and files where the contract depends on kernel
  behavior. Use explicit barriers to order races on both sides of the relevant
  commit or ownership change. Use bounded waits for observable state and keep
  failure logs. Run deliberately broken guards only in an isolated copy and
  verify the intended assertion fails after the baseline passes.

## Integrated workflows and failure cases

Each scenario uses normal entry points and compares visible outcomes with
destination or recovered bytes as applicable. The prior stage links supply the
complete contracts and focused cases.

### V01 — Edit and navigate (FR-01, FR-02, FR-03, FR-05)

Open two files with the CLI and one with the picker. Open the picker by
keyboard and through Actions with the mouse. Exercise directory navigation and
file activation on both routes, and open a file by literal path. Switch tabs
with keys and mouse. Place the caret and drag across literal tab characters,
combining text and wide glyphs, autoscroll, copy/cut and paste internally, then
bracketed-paste external text. Resize while selecting and verify hit targets
and positions. Undo and redo. Save and compare exact bytes and newline
conventions. Verify failed opens distinguish binary, unsupported encoding,
invalid UTF-8 and directory input and preserve existing tabs. At 40×8 exercise
Actions, Save/Save As, and dirty Close/Quit prompts with keyboard and mouse.
Cancel Close and Quit and verify the tab remains dirty and protected on the
next attempt.

[Stage 03 editing](03_editing_and_mouse.md#acceptance-criteria)
[Stage 04 tabs and picker](04_tabs_and_file_picker.md#acceptance-criteria)

### V02 — Guarded save and refresh (FR-02, FR-06)

On native macOS and Linux, open two clean files. From a second process,
exercise in-place and rename saves on both the active and inactive tabs. Wait
for tab-specific completion feedback before sending input, switching tabs or
changing focus. Verify the active tab and focus stayed in place, the refreshed
bytes and latest mapped view are correct, and undo restores the previous text
as dirty while redo returns to clean. Repeat dirty and with a held refresh that
started while clean, then edit before releasing it. The dirty revision
survives, the warning remains, and guarded Save preserves the externally
written destination. Also test a missing or invalid replacement that returns to
valid text. At save, force permission or space failure and a conflict
introduced before publish. Check the original destination, retry or Save As,
and preserved unsaved bytes. If publication can have happened, verify the
actual destination and the uncertainty message.

[Stage 02 storage](02_document_storage.md#acceptance-and-evidence)
[Stage 05 refresh](05_scratch_recovery.md#automatic-refresh-of-named-tabs)

### V03 — Recover scratches (FR-10, FR-09)

Use multiple scratch tabs with different selections/views and an active
scratch. Observe the default five-second checkpoint schedule and apply a
shorter interval through Settings. Quit after further edits before the next
periodic write. Relaunch and check text, tab order, active scratch, view state,
and fresh undo history. Repeat after saving one scratch as a named file and
explicitly discarding another. For crash recovery, publish checkpoint A, hold
checkpoint B before catalog publication and kill the editor. Verify recovery of
A without mixed content. Interrupt after publication as a separate case and
check the newly committed catalog. Inject checkpoint storage failure during
Quit and verify that the editor offers recovery and preserves current text
through retry or Save As. Reuse two-owner tests to verify a live owner cannot
be claimed by another instance.

[Stage 05 recovery](05_scratch_recovery.md#acceptance-criteria-and-test-plan)
[Stage 09 settings](09_configuration_and_keybindings.md#acceptance-criteria)

### V04 — Search and replace (FR-02, FR-06, FR-07, FR-08)

Put literal markers across storage boundaries, in a scratch with unsaved edits,
in a named tab, and in active stdin. Use both keyboard and mouse to open the
search controls, change current/all-tab scope, case and whole-word options, and
run or cancel an action. Verify wrap feedback and exact match positions. Use
Replace Next and Replace All and verify the reported processed and changed
counts against actual edits. Check that Current replacement is disabled in
active stdin and that the All Open summary counts the read-only tabs it
skipped. Undo once in each changed document and verify unchanged disk bytes
before saving. Cancel at a controlled boundary between documents and exercise
a failure there. Require the summary and actual edits to agree. After stdin
EOF, include the promoted scratch in replacement.

[Stage 06 search](06_search_and_replace.md#acceptance-criteria-and-validation-plan)
[Stage 08 streams](08_pipes_and_follow.md#acceptance-criteria)

### V05 — Highlighting (FR-04, FR-06, FR-09)

Verify offline detection and visible styling for every bundled language and the
embedded fixtures, including a neutral code body and styled delimiters and
label for an unknown fence. Change language and reload a custom theme,
association and query. Exercise logical sizes at and above 20 MiB and lines at
and above 64 KiB, including an offscreen long line. Isolate one limit at a
time. Use a controlled analysis timeout to test the 250 ms default, stale
result rejection, visible whole-document fallback, and continued edit/search.
Apply a lower limit through general settings, verify it affects eligibility,
then restore the default. Use focused tests for worker budgets and limit
equality, with candidate TUI checks for the visible fallback and continued
editing.

[Stage 07 highlighting](07_syntax_highlighting.md#acceptance-criteria-and-validation-plan)
[Stage 09 settings](09_configuration_and_keybindings.md#acceptance-criteria)

### V06 — Pipes and file following (FR-01, FR-07, FR-08, FR-10)

Gate a producer so partial text is visible before EOF. Split a UTF-8 scalar and
CRLF across writes. Pause viewing, observe continued ingestion, search retained
history, and resume. Attempts to edit, save or replace active text leave it
unchanged. On valid EOF the same tab becomes editable. Make an edit, use Save
As, and verify exact bytes. In a separate run Quit without Save As and recover
the promoted scratch. Use `--follow` for append, rename/recreate, and observed
truncation, checking boundary status, retained/exported bytes and untouched
sources. Induce quota/space or producer failure and verify the retained prefix
and export. In separate trials Close the active stream while a companion tab is
open and Quit with active input. After confirmation both release the pipe
reader promptly. After Close the companion tab still accepts input in editor
modes. After Quit the shell terminal modes are restored. Cover idle and busy
input. Redirect interactive stdout or remove the controlling tty in focused
process tests. Verify that startup fails before document data are consumed and
that no controls reach stdout.

[Stage 08 streams](08_pipes_and_follow.md#acceptance-criteria)

### V07 — Configure and recover (FR-01, FR-03, FR-09, FR-10)

Remap Save and a surface-specific action using keyboard and mouse in Settings,
check effective shortcuts and relaunch. Edit the TOML and reload to verify
agreement with the UI. Exercise a conflict and a malformed file, confirming
last-good settings on reload and defaults plus a diagnostic on initial load.
Introduce a stale Settings draft through an external write. Verify Save
Settings preserves the external file and reload allows a fresh save. Launch
with `--safe-config` and confirm portable controls, normal editing and recovery,
and unchanged user configuration. Verify discoverability of commands and
effective bindings and coverage of all remappable command IDs against the
catalog.

[Stage 09 settings](09_configuration_and_keybindings.md#acceptance-criteria)

### V08 — System clipboard toggle (proposed, FR-03 and FR-09)

Use a disposable config to toggle, save, relaunch, reload, and restore defaults.
Verify a changed or reset draft takes effect only after save. Test a malformed
value and safe startup. A real PTY must emit no OSC 52 write in internal-only
mode, on an empty selection, or after a failed Copy or Cut. In enabled mode
decode the frame and compare the bytes with a selection containing a wide
character and newline. Check internal-only, request-sent, and skipped or
failed status messages. Copy from active stdin through its controlling tty.
Cut there is rejected and emits nothing. Check that Cut on an editable tab
publishes only after deletion commits. Simulate a rejected write before
transmission and verify internal Paste still recovers the text. Inject failure
after the OSC introducer and verify abort/recovery or terminal-loss handling
before unrelated output. Exercise the
payload limit at and above the boundary. Use controlled ordering to complete
a Copy started while disabled after enabling the toggle. Then hold an enabled
Copy while disabling and re-enabling. Neither held command may send a frame.
Exercise pending-write revocation on newer Copy and suspension. Run the client
clipboard sequence below with the actual frontend and tmux/SSH path.

### V09 — Empty session, tab strip, and picker (proposed, FR-03, FR-05, FR-10)

Launch with an empty state profile and no paths. Verify zero tabs, visible
Open/New actions, and no scratch created on Quit. Repeat with failed CLI
opens and confirm their errors in the welcome view. Relaunch with retained
scratches and confirm their order and view are restored without an extra
scratch. Repeat with failed CLI opens alongside successful scratch recovery.
Verify the errors stay visible and the restored scratch is active. Open and
create from the welcome screen with keyboard and mouse, using both defaults
and remapped shortcuts.
Close the last tab and repeat. At regular size and 40×8, switch tabs, inspect
dirty/error indicators, and access tabs outside the strip. In the Open picker
verify single-click selection, activation after a timed double-click of the
same file or directory, and keyboard activation. Spaced clicks select only.
A third click cannot activate again or carry a pending click into the next
listing. Verify no accidental activation across a resize, drag, or replacement
listing.

For changed data-protection guards, add or extend the focused regression first,
show the expected pre-fix failure, then record the passing test and the
deliberate guard break and assertion failure. Reuse existing focused tests where
they reproduce the regression. A review of source alone is insufficient for a
filesystem race or real terminal behavior.

## Terminal and transport checks

FR-01 and FR-03 require real frontend evidence in addition to executable PTY
tests. Use the following proposed reference matrix. A direct path has no tmux
layer. For remote tmux rows, tmux runs on the editor host. Record any additional
client-side nesting separately if exercised or documented as supported.

| Row | Client → editor | Required run |
| --- | --- | --- |
| T10-1 | macOS iTerm2 → local macOS | Base sequence in legacy and enhanced input profiles. Demonstrate direct Cmd/Super delivery and a tested Cmd-to-Ctrl mapping, with portable alternatives. |
| T10-2 | macOS iTerm2 → local tmux → macOS | Base sequence and tmux sequence in the portable profile. Test enhanced delivery where the path supports it and record actual behavior or fallback. |
| T10-3 | Native Linux graphical terminal → local Linux | Base sequence and portable operation. Reference frontend may be xterm or GNOME Terminal, with actual version recorded. |
| T10-4 | Same Linux frontend → local tmux → Linux | Base sequence and tmux sequence. |
| T10-5 | macOS terminal → SSH → Linux | Base sequence and SSH disconnect/reconnect sequence using an allocated remote PTY. |
| T10-6 | macOS terminal → SSH → remote tmux → Linux | Base sequence, tmux sequence and fresh SSH connection followed by reattach to the same editor. |
| T10-7 | macOS terminal → mosh → Linux | Base sequence and interruption/resumption sequence, without tmux. |
| T10-8 | macOS terminal → mosh → remote tmux → Linux | Base, tmux and interruption/resumption sequences. |
| T10-9 | macOS terminal → Eternal Terminal → Linux | Base sequence and interruption/resumption sequence, without tmux. |
| T10-10 | macOS terminal → Eternal Terminal → remote tmux → Linux | Base, tmux and interruption/resumption sequences. |

Use profiles representative of intended operation. For each row record client
and editor OS/architecture, terminal application and version, terminal profile,
`TERM`, negotiated keyboard mode or portable fallback, tmux settings/version,
transport client/server versions as applicable, and any required upstream
settings. Record actual dimensions and the method and length of any connection
interruption. A version tested on one path establishes evidence for that path.

1. **Base sequence.** Enable IXON in the disposable shell before recording
   `stty -g`. Open a disposable text file and scratch in a window at least
   40×8. Verify portable editing, navigation, Ctrl+S saving with flow control
   disabled in ede, Find and Quit. Resize below 40×8 with unsaved text, then
   restore a supported size and check redraw, view state, and access to the
   unsaved-change prompt. Click, drag, and scroll
   after resize. Compare internal copy/paste with terminal clipboard paste and
   check saved bytes. Pause ede mouse capture, select using the terminal or tmux
   copy mode, then turn capture back on with Actions. Verify that keyboard
   actions and the internal clipboard remain usable if upstream clipboard or
   mouse delivery is unavailable. Suspend with Actions and repeat input after
   `fg` where job control is available. Verify the shell's exact termios,
   normal echo, cursor behavior, and release of mouse, paste and acquired
   keyboard modes on suspension and Quit. On native PTYs also check recoverable
   startup/exit errors and signal cleanup.
2. **tmux sequence.** With a dirty fixture and a selection, detach, change the
   window size and reattach to the same session. Check the retained document,
   dirty state, refreshed geometry, current input/capture mode, and save
   bytes. Exercise native tmux copy mode and return to ede. Use a disposable
   tmux server so the test cannot close a working session.
3. **SSH disconnect/reconnect.** Save marked named-file bytes and acknowledge a
   scratch checkpoint on the editor host. Add unsaved markers to both. Disrupt
   the disposable connection and establish a fresh SSH connection with a new
   PTY size. In direct SSH, verify working input and clean modes in the new
   shell and the last saved named-file bytes. Observe the original PTY and
   process from a companion shell with a bounded wait. If it outlives the
   interruption, record that behavior, then force definitive connection loss
   by ending the isolated SSH client and verify the original editor exits
   once its PTY is torn down. If transport teardown is inconclusive, exercise
   definitive tty loss separately on a controlled PTY. Once the old owner
   retires, launch ede in the fresh connection and verify the last completed
   scratch checkpoint. Observe checkpoint status or use a controlled pause in
   checkpoint publication to identify the exact recoverable revision. In the
   tmux row, reattach to the same editor through the fresh connection and
   verify its unsaved markers, current geometry and ability to save.
   Terminal-loss behavior follows
   [Stage 08](08_pipes_and_follow.md#close-quit-suspension-and-terminal-loss).
4. **Interruption/resumption.** During a live mosh or Eternal Terminal session
   with unsaved text, interrupt and restore connectivity without ending the
   editor's remote session. Change the client size while disconnected. On
   resumption verify the same editor resumes with its dirty text, redraws at
   the current size and handles keys, hit tests and paste. Save and compare
   exact bytes. In the tmux rows also exercise explicit detach/reattach. If the
   session fails to resume, record the failure and whether a fresh connection
   can recover the tmux session or the last scratch checkpoint.
5. **Enhanced keys.** On a named, versioned enhanced macOS profile, demonstrate
   at least one direct Cmd/Super action through a physical shortcut. Exercise
   one Cmd-to-Ctrl mapping end to end in an isolated profile and document its
   exact settings. Verify portable operation with enhancements disabled.
   For each advertised Cmd alias, use the physical shortcut on the claimed
   path and check its corresponding portable behavior, including modifier and
   focus distinctions. Record interception and the verified portable or
   Actions fallback on direct, tmux and macOS-to-Linux paths. Changing a user's
   terminal or tmux configuration is outside the editor's responsibility.
   Include the reported Terminal.app → SSH → Ubuntu path and the reported
   iTerm2 path through tmux, recording where the tmux server runs. Try Cmd+S
   with a dirty fixture and check saved bytes. Apple's default Terminal.app
   shortcut exports the terminal's text. Record such terminal actions and
   verify Ctrl+S or a tested profile mapping saves the file in ede. If a
   Cmd event reaches ede and fails to dispatch, reproduce and fix it.
   [Apple Terminal keyboard shortcuts](https://support.apple.com/en-mn/guide/terminal/trmlshtcts/mac),
   [iTerm2 profile key mappings](https://iterm2.com/documentation-preferences-profiles-keys.html).
6. **Client clipboard (if the toggle is accepted).** Try an ordinary Copy in
   Terminal.app over direct SSH to Ubuntu and in iTerm2 over direct SSH and
   through the reported tmux topology. Include an iTerm2 local baseline.
   Use distinct markers for the existing macOS clipboard and the selection
   in ede. In internal-only mode, paste into a Mac GUI app to verify its old
   clipboard and use Ctrl+V in ede to verify the new internal copy. With the
   toggle enabled, verify exact pasted text in the Mac app on a supported
   direct SSH path and a supported tmux path, including a committed Cut and
   a selection at the documented limit. Use the terminal Paste action to
   send a new Mac marker to ede and verify that it inserts once. Where the
   path discards OSC 52, verify internal Paste and a terminal-selection or
   tmux copy-mode route. Record the actual outcome before listing a path or
   payload size as supported.

A PTY that injects mouse or keyboard sequences can establish ede's decoding
and cleanup behavior. A passing row in this matrix requires the actual frontend
and transport and evidence for its stated actions. Mark upstream limitations
at the affected action, including the verified fallback. Record inaccessible
rows as blocked and resolve them or obtain explicit user approval for the
coverage change before stage acceptance.

## Performance and resource checks

Reuse the release probes listed in the [README](../../README.md#development-and-benchmarks).
Run the executable through a real PTY for measured local input and rendering.
Use storage and coordinator probes for phase timing and managed allocations.
Keep per-run output and label the scope of each observation.

### B10-1 — 1 GiB baseline

Run `large_file_interactive_benchmark` with `EDE_BENCH_MIB=1024` and
`EDE_BENCH_CORPUS=ordinary`, `dense` and `long`. Use a host or VM with about 8
GiB usable RAM, or an explicit effective 8 GiB limit with documented host/cache
conditions. Verify first text, editable Ready, tab switch, beginning/middle/end
editing, and save while accepting another edit. Check saved output and
dirty-state protection. Use `storage_bench` for undo/redo, full output
comparison, copy-path costs and clone results where available.

### B10-2 — Larger than RAM

Run a fully allocated ordinary-text file strictly larger than the RAM assigned
to a dedicated host or VM. Record usable RAM and disable swap in that test
environment. Use the interactive file probe or an equivalent executable PTY run
with an explicit MiB size. Verify open, navigation, beginning/middle/end edits
and save while preserving newer edits. Run literal Find, Replace All and Undo
using `large_search_and_replace_benchmark` or the same-size marked corpus with
a known match count. Compare saved output in bounded chunks against an
independently generated expected stream, covering the entire edited and
replaced files and all unchanged ranges. The current PTY probes sample
positions and length, so supplement them for full comparison. A cgroup-only run
on a larger host addresses the separate constrained-editor check.

### B10-3 — Constrained editor

On Linux, run the 1 GiB ordinary, dense and long interactive and search/replace
probes with the existing 256 MiB cgroup and zero swap. Check the actual applied
limits, completion without OOM, byte integrity and responsiveness. Include the
1 GiB recovery/refresh probes and the existing large highlighting/search
overlap. Verify fixtures and working files use disk-backed storage, not tmpfs.

### B10-4 — Sustained streams

Use `sustained_piped_capture_benchmark` with 1 GiB of generated ordinary, dense
and long-line text. Time first text with a gated producer, keep a companion tab
editable while capture runs, search the retained text before closing the
producer and time EOF adoption separately. Run the 8 GiB baseline and a Linux
256 MiB swap-disabled run. Cover sustained capture, raw partial input and
retention/error behavior with V06 in addition to this measurement.

Run the 1 GiB interactive, search and stream probes on both native platforms,
with the 8 GiB baseline and swapless checks documented separately. Record an
unavailable required environment as blocked. Include both first-pass and
repeated measurements for B10-1. State how cache conditions were obtained.
Just-generated fixtures are warm. Label uncontrolled cache conditions as such.
Use fully allocated valid text and bounded fixture generation and
verification. Provide enough filesystem space for source,
snapshots, indexes, edit/recovery storage, and output. Record snapshot clone
versus copy where observed, and exercise the forced-copy path.

- **Local goals.** First usable text within 1 second and ordinary input response
  within 50 ms on the baseline. First display runs from launch until a document
  marker is visible in the PTY. Record editable Ready, index, Save and
  search/replace completion separately. Ordinary input runs from the time an
  event is available at the PTY to its first visible effect. Use the interactive
  probe's 40 ordinary edit samples per run and record sample count, p50, p95,
  and maximum. Sample a ready companion tab during sustained ingestion and
  other long work and confirm it overlaps the sample interval. If ingestion
  completes before the samples, repeat with a controlled producer schedule
  and record its pacing separately from the full-speed throughput run.
  During indexing, full search, Replace All preparation and Save staging, verify
  visible progress and exercise navigation or editing in a Ready companion tab.
  Hold work at a known pre-commit boundary where the existing probes cannot
  prove overlap. Exercise Escape when cancellation is safe and verify that
  unpublished changes leave original bytes intact. Use these controlled runs
  to establish ordering separately from unmodified throughput runs.
  Compare maxima with the goal and report individual misses. Include
  representative navigation and mouse event-to-visible samples alongside
  typing. A change to an agreed goal requires explicit user approval.
- **Resource evidence.** Record CPU, architecture, OS, RAM, swap, limits and
  filesystem/storage for all results. Record corpus shape and allocated/actual
  bytes, terminal geometry, cache state, build profile, workload and sample
  cadence. Collect editor kernel-peak RSS where available and label sampled RSS
  separately. Record cgroup peak (including charged cache), CPU time, FD
  high-water, managed cache/queue/index/history charges relevant to the probe,
  and logical/allocated working-file high-water for snapshots, indexes,
  recovery and stream spools as applicable. Compare bounded resources against
  the existing stage budgets and explain any overrun. Report clone-sharing
  caveats with allocated disk counts. Evidence can come from complementary
  probes with their scope recorded.
- **Remote timing.** For each transport record connection conditions and a
  representative input/resize response at the client. Keep client-visible
  timings distinct from editor processing and PTY timings. Use client and
  host probes separately when available and state what the clocks measure.
  Keep the 1 s and 50 ms checks on the local baseline.

## Build and completion gates

1. On native macOS and Linux run `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets --locked -- -D warnings`, and
   `cargo test --workspace --locked` with the pinned toolchain. Run the opted-in
   benchmark and tmux cases explicitly. Record OS-specific skips and both CI
   job links with the tested SHA. The current draft-PR workflow provisions only
   macOS on a draft event. Obtain the full matrix for the candidate on the
   implementation PR's ready-for-review run or a subsequent update to that PR.
2. For every required check, record **pass**, **fail**, **blocked**, or a
   user-approved disposition. Keep earlier misses and the result of any rerun.
   Record broken behavior with the exact environment, trigger, and observed
   outcome. Fix regressions and rerun affected checks. If the requirement or
   approved coverage must change, record the user's decision and update the
   overview and affected stage contract. Missing equipment and an earlier
   stage deferral leave the Stage 10 check open.
3. Update the README with locked source-build prerequisites, first-run edits,
   saves and scratch behavior, pipe/follow examples, configuration paths and
   safe startup, portable controls, clipboard modes and fallbacks, known limitations,
   and the tested terminal/version/profile/transport combinations. Link to the
   evidence here. Verify the commands in a fresh isolated checkout and profile
   on each OS and the documented terminal-specific instructions in their
   respective matrix rows.
4. Use development prereleases during work as needed. Set the release candidate
   Cargo package version and corresponding lockfile entry to `0.0.1`. Verify
   locked release build, `ede --version`, the version shown by `--help`, and
   README version against that candidate. Record evidence against the exact
   final executable inputs. Rerun checks affected by later changes before
   recording completion.
5. Have an independent reviewer follow [REVIEW.md](../../REVIEW.md) against
   the candidate and feature base. Supply acceptance traces, test output,
   benchmark conditions, and guard-mutation evidence when applicable. Fix
   findings and have the reviewer inspect material fixes.

## Acceptance criteria

| ID | Completion evidence |
| --- | --- |
| MVP-01 | All V01–V07 observable outcomes have candidate evidence and trace back to FR-01–FR-10. Refresh of active and inactive clean tabs and protection of dirty revisions and external bytes are verified before release. |
| MVP-02 | Required T10 matrix rows pass or have an explicit user-approved coverage disposition. Native, remote, and PTY results identify the actual path and verified fallbacks. |
| MVP-03 | B10-1 through B10-4 have results with measured latency, memory and disk behavior. The 1 GiB / 8 GiB baseline and larger-than-RAM runs are evidenced independently. Missed goals or exceeded budgets are fixed and rechecked or resolved by user-approved requirement changes. |
| MVP-04 | Native locked checks, opted-in regressions, and independent review for the candidate are recorded. Required failure and controlled ordering evidence is present. |
| MVP-05 | A fresh source build and documented workflows work as described, and Cargo metadata, the executable and README agree on `0.0.1`. The validation record states supported paths, observed limitations, and approved dispositions. |
| MVP-06 | Record candidate evidence for each accepted pre-release proposal. Clipboard acceptance uses V08 and the client clipboard sequence, with README guidance for the toggle, default, tested paths, and fallbacks. Use the applicable V09 cases for each accepted UI change. |

## Validation record

Integrated implementation and release validation are **pending**. Record the
candidate SHA, commands or manual steps, environment, results, and logs for each
applicable V, T10, B10 and MVP criterion here as checks run. Keep design review
evidence and actual implementation evidence identifiable by revision. No new
coverage deferral has been approved.

- Independent design review against `dfacfe2` found gaps in terminal and mouse
  coverage, stream Close and Quit, refresh, syntax fallback, larger-than-RAM
  search, and validation of progress during overlapping work. Follow-up review
  of the material fixes found no remaining actionable findings.
- On macOS 27.2 arm64 with Rust 1.99.1, the merged Stage 09 baseline passed
  `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets --offline --locked -- -D warnings`,
  and `cargo test --workspace --offline --locked` with a 256-descriptor soft
  limit. All 313 enabled tests passed. Ten opt-in tests were skipped. The
  passing run used normal terminal permissions after the sandbox denied access
  to the controlling tty in seven pipe tests.
- Independent review of the proposed pre-release changes against `77157ae`
  checked clipboard authorization, incomplete terminal writes, empty-session
  errors, picker click ordering, and applicability of the new checks. Follow-up
  review of the fixes found no remaining actionable findings. Formatting,
  Clippy, and the 313 enabled macOS baseline tests passed again. Real clipboard
  and shortcut behavior remains to be verified on the reported terminal paths.
