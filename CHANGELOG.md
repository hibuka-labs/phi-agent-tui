# Changelog

All notable changes to phi-agent-tui.

## [Unreleased]

## [0.5.0] - 2026-11-20

### Added
- **The picture labels ride into history.** The capture path
  (`<image name=N path="...">`) follows the pixels into history, so an
  evicted picture stays recoverable — the window-reset residue parser reads
  the path fields back out.

### Fixed
- **A panic restores the mouse.** The panic hook now runs the full terminal
  restore (raw mode, mouse reporting, alternate screen). It is the only
  restore that runs when a panic aborts, and skipping the mouse resets left
  the emulator emitting mouse-report fragments into whatever shell inherited
  the terminal.

### Changed
- **Breaking (dependency):** `phi-agent` requirement bumped from 0.23.0 to
  0.24.0, `phi-kernel-tools` from 0.15.0 to 0.16.0 and `phi-telemetry` from
  0.9.0 to 0.10.0.

### License
- **Dual-licensed as MIT OR Apache-2.0.**

## [0.4.1] - 2026-11-16

### Fixed
- **The refused-keyboard-pop test no longer fails on Windows.** The test
  modelled the legacy console API's refusal as a failed *write*, but the
  API refuses the kitty keyboard pop *before* it is ever written — so on
  Windows the escape sequence never reached the writer and the mock could
  not see the attempt. The test now asserts the observable invariant that
  holds on every platform (the alternate screen is still left behind the
  refused pop) and adds a Windows twin that pins the console-API contract.

## [0.4.0] - 2026-11-16

### Fixed
- **The exit replay defaults to the summary.** Replaying what the session
  ended with showed every line instead of the summary the user actually
  wants to read.

- **The kitty keyboard push is best-effort, not a startup gate.** A
  terminal that does not support the keyboard protocol used to stop the
  session from starting. It now starts, and the push is skipped.

- **Ctrl+H deletes backward instead of typing `h`.** The composer read
  Ctrl+H as a character.

### Changed
- **Breaking (dependency):** `phi-agent` requirement bumped from 0.22.0
  to 0.23.0, `phi-kernel-tools` from 0.14.1 to 0.15.0 and `phi-telemetry`
  from 0.8.0 to 0.9.0, keeping the family on a single version of each
  crate.

## [0.3.0] - 2026-11-14

### Added
- **Attention lines get their own face.** A `push_notice_line` path wraps
  engine notices as amber `LineKind::Notice` lines — the counterpart of the
  red error line, so a warning that means "look at this" no longer reads as
  "something broke". An engine notice that arrives while the run is still
  settling waits for the turn to land before it is shown, so the user sees
  one settled transcript instead of a line that jumps.

### Changed
- **Breaking (dependency):** `phi-agent` requirement bumped from 0.21.0 to
  0.22.0, `phi-tui` from 0.4.0 to 0.5.0, `phi-kernel-tools` from 0.13.0 to
  0.14.0, `phi-telemetry` from 0.7.5 to 0.8.0 and `log-core` from 0.1.4 to
  0.1.5, keeping the framework on a single version of each family crate.



## [0.2.0] - 2026-11-09

### Added
- **Two death sweeps for the task rows, with their exemptions** (Rev 2026-11-04).
  The all-complete sweep fires when nothing is running and no failure is
  unacknowledged; the turn-end sweep fires at the root turn end, so a corpse
  never crosses a turn. The pinned row is exempt — the inspection freeze keeps
  the row the user is reading alive past its death point.
- **Typing takeover**: typing, Enter or paste hands the keyboard back to the
  composer, so the reply is readable on the root transcript. The list's own
  keys (↑↓/Esc/Ctrl+T/Home/End/PgUp/PgDn) never steal focus.

### Changed
- **The task face shows live rows instead of a count strip** (Rev 2026-11-04).
  The middle panel renders the rows themselves — running rows, an
  unacknowledged failure as a red remnant, and the pinned row. Rows are keyed
  by id (`FocusTarget::TaskList(id)`), so focus follows the row across a
  re-render instead of an index that a dying row shifts. The frameless live
  face is capped at 6 rows and reuses the framed row render with the files
  column stripped.

- **Character-level mouse selection.** Press and drag selects exactly the
  characters under the pointer — not whole lines. The press snaps to the
  first cell of the glyph (a wide CJK glyph selects whole, not half), a
  double click takes the word, a triple click the block, and the right-click
  menu copies the selection. Highlighting paints the selected cells, not the
  whole row.
- **A screenshot can be pasted into the composer** — the picture rides the
  wire request as an image, behind a capability gate that asks the model
  registry whether the live model can see it.
- **Input history.** The composer keeps what was sent and brings it back with
  the history keys, filtered to the current workspace.

### Changed
- **Breaking (dependency):** `phi-agent` 0.21.0, `phi-tui` 0.4.0,
  `phi-kernel-tools` 0.13.0.
- The app, the render pass and the run loop moved into directories
  (`app/`, `render/`, `run/`); internal reorganization, no API change.

## [0.1.0] — 2026-11-02

First release: the terminal-app shell extracted from phimint's TUI as a
reusable framework crate.

### Added

- `ShellSpec` (data config) + `ShellHandles` (live objects) wire-up and
  `run(spec, agent, handles)` — the product assembles its own
  `phi_agent::PhiAgent`; the shell never builds an agent.
- Event loop with input pump, turn submission discipline (Esc/Enter duality,
  queued steering, held slash commands), and periodic self-heals (terminal
  size, raw-mode).
- Transcript translation and rendering over `phi-tui`, with the mouse
  hit-testing protocol and a semantic `Theme` (class-name tokens resolved at
  render time; defaults are the reference product's palette).
- Approval/ask interaction machinery: queue, popup, timeout, Esc/Enter
  discipline, two-key consent gate. Mechanism only — grading tables and risk
  wording stay product-side (`CopyTable` / `ApprovalCopy` wording slots).
- Session picker skeleton and resume support behind `Option<SessionIo>`
  (`None` = the product is not session-based: `/resume` has no row, no
  picker, no replay).
- Panel tiers: shallow event synthesis (synthetic turns with the
  idle-only timing invariant), middle layout slots (tips band, status-band
  face), deep view modes (`task_panel`, pickers).
- Synthetic-turn delivery policies: `bg_wake` (background-task auto-wake with
  hold/batch/quiet-window/report-once) and `child_results` (child-report
  fan-in batching).
- Product hook slots (update check, tips, notices, title generation, skill
  bridge, submit/reopen/session boot/end, panel tick) — every hook runs
  behind a guard: a panicking hook degrades to a documented fallback and
  leaves a flipbook trace instead of killing the event loop.
- Flipbook instrumentation (`frame_log`), exit transcript replay
  (`exit_dump`), and Unix termios self-heal (`term_guard`).

[Unreleased]: https://github.com/hibuka-labs/phi-agent-tui/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/hibuka-labs/phi-agent-tui/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/hibuka-labs/phi-agent-tui/compare/v0.3.0...v0.4.0
[0.1.0]: https://github.com/hibuka-labs/phi-agent-tui/releases/tag/v0.1.0
