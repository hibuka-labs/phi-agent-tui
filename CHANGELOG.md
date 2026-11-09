# Changelog

All notable changes to phi-agent-tui.

## [Unreleased]

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

[Unreleased]: https://github.com/hibuka-labs/phi-agent-tui/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/hibuka-labs/phi-agent-tui/releases/tag/v0.1.0
