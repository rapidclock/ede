# 11 — Extras

**Implementation candidate.** Branch `stage/extras`, PR target `feat/initial_mvp`. The baseline is merged Stage 10 PR #10 at `c9121b8`. The user authorized implementation of this stage. Automated results and open desktop acceptance checks are in the [validation record](#validation-record).

## Clipboard

**Decision.** Ede Copy always retains the full editor selection, including offscreen text, for the running session across tabs. It also attempts to copy to the user's desktop. There is one Copy and one Paste command. Remove the Internal/System/Both chooser and the proposed “Paste from ede” action. Select the route automatically.

| Where the command runs | Ede Copy | Ede Paste |
| --- | --- | --- |
| Direct macOS or Ubuntu graphical desktop | Retain the selection and write plain text to the desktop clipboard. | Read the current desktop clipboard on explicit Paste. Copying A in ede, then B in TextEdit or Ubuntu Text Editor, must paste B. |
| SSH, tmux (including local tmux), mosh, Eternal Terminal, Windows Terminal with WSL, or uncertain locality | Retain the selection and attempt one OSC 52 write to the client terminal. | Paste the retained selection, even across tabs and even if the terminal ignored, rejected or could not accept the write. With no retained selection, show “Nothing copied in ede” and leave the document intact. |
| Safe configuration or an existing explicit external-clipboard opt-out | Retain the selection privately. | Paste the retained selection. Ede initiates zero native clipboard operations or terminal clipboard requests. |

Terminal-owned Paste always accepts the text the terminal supplies through the existing input path. Thus a delivered Ctrl+V in remote ede pastes the retained selection, while Apple Terminal's ⌘V can paste newly copied Mac text into the same session. Windows Terminal may handle Ctrl+V itself. A terminal-supplied paste inserts once. Preserve bracketed-paste and field-input rules. Document actual shortcut delivery for each tested terminal.

**Locality and failures.** Use the macOS general pasteboard and the Ubuntu desktop clipboard (Wayland or X11 CLIPBOARD). Positively associate the terminal, user, display and seat with the local session. Forwarded or inherited display variables alone are insufficient. A directly launched Ubuntu VM terminal targets the guest desktop. Test host–guest exchange separately with working UTM clipboard sharing. In an identified local session, empty/non-text clipboard data or failed native reads preserve document, selection and history and give the reason. Never silently insert an older retained copy. Read only on explicit Paste. Keep payloads out of logs. Backend I/O must be cancellable and off the UI thread.

**Copy and Cut.** Retain only complete selections. No selection, failed staging or a cancelled operation leaves the previous retained copy intact. Keep the 8,192-byte UTF-8 cap on OSC 52 only. Above it, skip frame preparation, retain the entire selection and report the limit. Native writes support larger selections and repeated desktop pastes, including after normal editor exit. Distinguish retained-only, native success and terminal request sent (delivery unconfirmed). Preserve serialized tty framing and partial-write recovery. Cut follows the selected route. On the terminal or private-only route, commit retention and deletion together. Only the terminal route then attempts an external write. On the native desktop route, stage the selection and obtain a successful desktop handoff before committing retention and deletion together. At commit, recheck the original tab, revision, selection, authorization, operation generation and cancellation. Failed handoff, failed deletion or stale authority preserves document bytes, selection, history and the previous retained copy. If the handoff already succeeded, report that the text reached the desktop and the selection was kept. Successful Cut and document Paste each undo in one step.

Use bounded buffers and private staging, preserve accepted UTF-8 including line endings, and honor existing validation and storage limits. A native read targets the original focus, document revision and caret/selection. Cancel a stale read before it edits. Order rapid Copy → Paste so Paste observes the completed write. Cancel a waiting native Paste after a failed write. Retire queued external transfers on newer Copy/Cut, revoked permissions, suspension or teardown. An older completion cannot overwrite newer data or feedback.

**Compatibility decision.** A missing setting, legacy System/Both and `copy_to_system = true` select automatic behavior. Preserve `internal` and `copy_to_system = false` as explicit opt-outs, including on reload and unrelated saves. Keep invalid/conflicting-key rejection and last-good reload behavior. Describe the opt-out in config help. Restore Defaults explicitly drafts removing that override, subject to Save. New default files need no clipboard key.

**Implementation.** macOS uses an AppKit helper for the general pasteboard. Linux uses X11 CLIPBOARD under Xorg or the GNOME Wayland session's Xwayland bridge. It checks the terminal's PTY ownership, active logind session and local socket peer before using the helper. X11 uses `x11rb` 0.14 with bounded INCR transfers and keeps the acknowledged owner serving after normal editor exit.

**Baseline evidence.** Stage 10 recorded successful Mac clipboard writes through iTerm2 over SSH and configured remote tmux. Apple Terminal ignored the tested outbound request even though terminal Paste worked. The first local iTerm2 copy needed a retry. In Ubuntu on UTM, the user reported a sent request and stale text outside ede. The destination and display protocol were unverified. These results motivate separate tests for editor retention, actual desktop delivery and terminal-owned paste. [Stage 10 clipboard evidence](10_mvp_validation.md#terminal-transport-and-client-clipboard-evidence)

## Dismiss the Actions menu

With ede mouse capture enabled, a completed primary click outside the F2/Menu popup dismisses it and returns focus as Escape would. Consume the entire gesture, including over a tab, its close button, Settings, Quit or the document. Only a subsequent click can act on the exposed surface. The popup border and blank interior count as inside. Preserve row activation, wheel scrolling and keyboard Escape. A drag across the border, release without a matching press, or a resize/modal change between press and release cannot activate or dismiss a different target. This rule is specific to Actions. Settings drafts, file pickers and destructive confirmations retain their own dismissal rules.

## Unified Settings pane

**Decision.** Give Settings its own permanently visible, labeled workspace control at supported sizes, including the welcome view and 40×8. One click opens it directly. Use F5 as the default for the existing remappable global Settings command. Keep explicit user bindings and unbindings. On load, reload and draft validation, an explicit F5 on a competing workspace command suppresses inherited Settings F5. Removing that conflict restores inheritance. Disjoint modal uses remain valid, and conflicting explicit bindings still require correction. Keep Actions → Settings for compatibility. The workspace control is reachable by keyboard even if the shortcut is unavailable.

Show a single scrollable list with Recovery, Highlighting limits and Keyboard shortcuts sections. Include checkpoint interval, maximum document and line sizes, parse timeout, and every configurable command by context, including unbound commands and all chords. Show labels, units, valid ranges, effective values and default/override state. Custom syntax themes, language rules and query files stay in their existing files, as requested. Show configuration path, source and diagnostics, with Reload and Restore Defaults accessible in the pane.

- Show multiple labeled rows whenever space permits. A click on a value edits it in place. A shortcut row offers add, replace, remove, capture, typed entry and reset to default. Preserve capture preview and conflict detection. Ordinary preference edits and saving require no Options page.
- Wheel, arrows, Page Up/Down and focus navigation reveal offscreen entries. Keep the focused control visible and the footer fixed. At 40×8 retain an editable viewport, context, focus and all three footer actions. Below minimum size preserve the draft and suppress obscured actions. Opening the pane or scrolling its body never edits the document.
- **Save** includes the current inline value (including a typed chord) without a separate Accept click, validates the whole draft and persists changes as one revision. A captured chord still requires explicit preview acceptance. Count pending field edits when deciding whether a draft is dirty. Keep the pane open and show the result. On pre-publication failure keep the draft and focus the offending row if applicable. **Discard** restores the current applied values and keeps the pane open. Confirm discarding a dirty draft with Keep Editing focused. **Exit** closes a clean pane and restores opener focus. On dirty Exit or Escape from browsing, offer Save and Exit, Discard and Exit, or Keep Editing, initially focused. Save and Exit closes only after confirmed application. Cancellation, validation failure or uncertain publication keeps the pane and draft accessible. Exit closes Settings only. Escape in an inline editor first cancels that edit.
- Preserve safe-mode restrictions, key capture safeguards, reload conflicts, TOML comments, stale-file protections, cancellable writes and post-publication uncertainty from Stage 09. During a save, show progress, prevent duplicate saves and retain the existing Cancel Save/Finishing save behavior. Existing remapped Settings commands still reach their corresponding operations. Keep Settings edits independent of document Save, Close and Quit. [Settings persistence and keyboard contract](09_configuration_and_keybindings.md#settings-ui-and-persistence)

## Tab strip

Replace the bracket-only boundaries and the single shared × with a visible one-cell vertical divider between adjacent tabs and an × **inside each visible tab**, immediately after its label area. For example, `notes × │ todo × │ build.log ×`. Omit dividers at the outer edges. Keep the header one row, use terminal cell widths for labels and preserve active, dirty, attention and focus indicators even without color. Reserve room for × and dividers before clipping names. At 40×8 prioritize the active tab and its ×, the Tabs overflow route and the direct Settings control. Compact or reposition other workspace controls as needed while preserving their functions.

Clicking a name selects its tab. Clicking a divider does nothing. Clicking × invokes the existing guarded close on **that tab's stable ID**, even for an inactive tab. Closing any inactive tab preserves the active tab and its view, including after Save As or Discard. Dirty, scratch and streaming tabs use their existing close/retention/confirmation rules. Confirmations and Save As identify and act on the original target. Cancelling restores the prior active tab and view. Closing the active or last tab uses the existing neighbor or welcome behavior. A drag, changed tab identity, resize or layout change between press and release cannot close a tab. Preserve keyboard Close Tab and the ability to select and close overflow tabs.

## Acceptance criteria

| ID | Required check |
| --- | --- |
| E11-1 | In direct Apple Terminal, iTerm2 and stock Ubuntu GUI sessions on Wayland and X11, round-trip distinct multiline markers (including 界) between another app and ede, change the external marker before Paste, compare exact bytes and undo once. Exercise multi-MiB Copy/Paste, empty/non-text/invalid input and normal exit after Copy. On UTM verify guest↔guest first, then guest↔Mac with a known-working GUI-to-GUI sharing baseline. Record versions, backend, dependencies, session and actual destination. |
| E11-2 | Exercise direct SSH, local tmux and SSH with remote tmux from both Mac terminals. Seed different editor/client markers. Verify cross-tab editor Paste after accepted, ignored, failed and oversized (8,193-byte) external requests, including after Cut. Test the 8,192-byte boundary, compare actual client pastes with captured frames, and verify terminal ⌘V separately. Repeat WSL → Windows Notepad and back in Windows Terminal with default bindings, and use a delivered editor command to verify retention. Record terminal settings, physical key delivery and forwarding. |
| E11-3 | Use controlled ordering for rapid Copy → Paste, stale reads, superseding writes, cancelled Cut, suspension and opt-out reload. Verify exact selection, retained bytes, history and final feedback. Inject storage exhaustion, native failure and partial tty writes. Test invalid and legacy configs, restore/save, safe mode, SSH with forwarded display and a local shell pointing at a different seat/display. Count reads, writes and queries: no native calls on terminal routes, no external clipboard operations in safe/opt-out, and no OSC 52 read queries anywhere. In direct safe/opt-out sessions verify Cut, cross-tab Paste and one Undo. Delay native Cut handoff across a selection change or revocation and inject deletion failure after handoff. Verify the original document and retained marker and report the actual desktop outcome. In isolation break each changed data-protection guard and confirm its intended assertion fails. |
| E11-4 | Mouse and real PTY at regular size and 40×8: click outside Actions over content, Settings, a tab × and Quit. Only Actions closes. Verify focus, selection and tab IDs. Exercise border clicks, drags in both directions, wheel, Escape, stale release after resize, capture off and independent guarded dialogs. |
| E11-5 | Starting with fresh config, open Settings in one click without opening Menu, and separately by F5. Test with zero and many tabs, explicit Settings remap/unbind, competing and disjoint F5 bindings, conflict removal and focus activation with mouse capture off. Edit a numeric value and remap a shortcut in one scrolled draft. Click the fixed Save button once while the last inline field still owns focus. Check invalid pending text on Save and Exit, cancellation, and failed/uncertain publication. Verify runtime values, actual config bytes and reopened values. Repeat keyboard-only and at 40×8. Exercise Discard, dirty Exit choices, safe mode, stale/concurrent file changes, save failure/cancellation and key capture. Verify custom syntax files and the edited document are untouched. |
| E11-6 | Render zero, one, two and overflowing tabs with short, long and wide names. Check divider cells, × per visible tab, monochrome active state, truncation and distinct hit regions at 40×8 and wider. Click inactive clean and dirty ×, cancel or save the named dirty tab, then close active and last tabs. Assert surviving IDs, active view, disk bytes and correct prompts. Exercise inactive Save As and Discard, an inactive live stream (no stop before consent), overflow Close and keyboard Close Tab. Test stale presses, drag and resize. Use barriers to order an in-flight save and a disappearing target against Close. |

Use focused red/green behavior checks, rendered terminal cells for layout, real PTYs for input and real desktop apps for clipboard delivery. Run formatting, Clippy and relevant regression suites during implementation. Update README and in-app Help for the shipped gestures and settings.

## Validation record

**November 18, 2026.** The Stage 11 candidate was tested against the Stage 10
baseline `c9121b8` using Rust 1.99.1 on arm64 macOS 27.2 (26B123) and in an
isolated arm64 Debian Bookworm container. The workspace test suite passed on
both hosts with a soft descriptor limit of 256. Formatting and Clippy passed
on macOS and Linux. The macOS tmux 3.8 smoke test and the Linux Xvfb clipboard
probe also passed when run explicitly.

| ID | Evidence and remaining acceptance |
| --- | --- |
| E11-1 | **Partial.** The Ubuntu Wayland guest passed bidirectional Unicode and multi-MiB clipboard tests with real graphical apps, exact byte comparison, one-step Undo, and repeated reads after normal ede exit. Xvfb exercised X11 ownership, peer rejection, empty selection, Unicode/CRLF and incremental reads. Injected backend tests exercised empty and invalid text. Direct Apple Terminal and iTerm2 automation, the stock Ubuntu Xorg desktop, and the UTM sharing baseline remain open. |
| E11-2 | **Partial.** Real PTYs exercised OSC 52 framing, the 8,192-byte limit, retained 8,193-byte legacy System copies, terminal-owned bracketed paste, tty backpressure and signal restoration. A separate macOS tmux 3.8 session exercised retained Copy/Paste and suspension. Actual GUI client delivery and physical shortcut handling in both Mac terminals, and Windows Terminal with WSL and Notepad, remain open. |
| E11-3 | **Partial.** Controlled tests exercised failed-write ordering, stale reads, superseding Copy, revocation after handoff and at storage commit, injected deletion failure, suspension waiting for a helper, and opt-out reload. The tests checked retained bytes, revision, selection and undo. PTYs confirmed cross-tab Cut and Paste with one Undo under the explicit opt-out and safe configuration. The real desktop locality cases and a full-disk native transfer still need validation. |
| E11-4 | **Partial.** Real PTYs covered outside clicks over Quit, Settings and a tab close at the supported floor and regular size, popup border and crossing drags, and preservation of the active view. The full pointer case matrix remains open. |
| E11-5 | **Partial.** PTYs and reducer tests covered direct entry, F5 and keymap inheritance conflicts, numeric keyboard and mouse edits at 40×8, direct Save of pending input, invalid Save and Exit, resizing with pending input, Restore Defaults, capture and its discard guard. Stage 09 persistence and configuration regressions passed. The full combined-draft and publication acceptance matrix remains open. |
| E11-6 | **Partial.** PTYs covered per-tab × and dividers, inactive dirty close with Cancel and Save, the named inactive scratch Save As at 40×8, preserved active caret and disk bytes, and overflow, active and last-tab close paths. The remaining rendered width and overlapping inactive stream/save cases need explicit checks. |

**Ubuntu desktop evidence.** Ubuntu 26.10 arm64 in UTM used GNOME Shell 51.0
on Wayland, Ptyxis 50.1 with VTE 0.84.1, and Ubuntu Text Editor 50.1.
The session was active on seat0 and the backend used its local Xwayland
CLIPBOARD. The first run exposed Ubuntu's `org.gnome.Shell@ubuntu.service`
unit name and fell back to OSC 52. After recognizing that exact session
service, Ctrl+C reported desktop success and Ctrl+V read the newer marker
copied by Text Editor. The paste buffer matched exactly when copied back
from Text Editor. Text Editor added a trailing newline when saving its own
file. ede saved the alternate marker exactly, and one Undo restored the
original bytes. A 3,538,949-byte selection copied to the guest desktop and a
3,407,877-byte selection pasted from Text Editor matched exactly. One Undo
also restored the original large document. Two desktop reads after normal
ede exit both matched the original small marker. The Mac locked before the
separate host–guest sharing baseline was run.

**Guard mutations.** In isolated copies, removing the storage cancellation
authority, retained-copy cancellation check, terminal frame generation
check, Settings discard focus guard, native suspend readiness gate, or
compositor service scope check failed the corresponding behavior assertion.

The separate implementation review traced native clipboard locality and commit
authority, Settings controls, and stable-tab close. Its follow-up confirmed
the corrected X11 credential ordering and suspension and Settings guards.
The final minor finding, naming the parked tab in recovery Save As, was
addressed. The earlier design review covered the same requirements against the
Stage 10 implementation. No desktop coverage deferral has been agreed.
