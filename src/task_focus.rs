//! Task-frame focus machine — the three-state toggle (design 20261103).
//!
//! S0 collapsed ⇄ S1 open-unselected ⇄ S2 open-selected. `frame_open` is the
//! single frame-visibility truth; S1 vs S2 is `focus`. Split impl of `App`
//! (methods live here, state stays on `App`): the toggle key, the list
//! navigation keys, the D10 landing rule and the frame's scroll window.
//! Lifecycle/counting bookkeeping stays in `task_panel`.

use crate::app::{App, SubAgentStatus};
use crate::task_layout::window_offset;
use crate::task_panel::TaskRow;

/// Where the keyboard focus is (re-exported through `app` for render/tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusTarget {
    /// Focus is on the input box (S0 collapsed / S1 open-unselected).
    Input,
    /// S2: the frame is open and this row is selected.
    TaskList(usize),
}

/// Task panel UI state — the three-state toggle machine (design 20261103):
/// S0 collapsed ⇄ S1 open-unselected ⇄ S2 open-selected. `frame_open` is the
/// single frame-visibility truth; S1 vs S2 is `focus`.
#[derive(Debug, Clone)]
pub struct TaskPanel {
    /// Current focus position.
    pub focus: FocusTarget,
    /// S0 when false; S1/S2 when true.
    pub frame_open: bool,
    /// First visible row of the frame's scroll window.
    pub window_offset: usize,
    /// S1-only window anchor: the failed row the auto-expand landed on
    /// (F5). `None` in S1 = anchor the tail (new work lands there).
    pub s1_anchor: Option<usize>,
    /// Session-cumulative terminal counts (the ledger markers are the
    /// source; rows may be reaped, these never drop).
    pub counts: crate::task_layout::TaskCounts,
    /// Failures not yet seen from the list — the `· k new` badge, cleared
    /// when the list is entered (D10).
    pub unacked_failures: usize,
    /// The auto-expand already fired this turn (at most one pop per turn).
    pub popped_this_turn: bool,
    /// The user manually collapsed this turn — no re-pop (F9).
    pub user_collapsed_this_turn: bool,
    /// Rows the frame last drew — window sizing + page steps.
    pub view_rows: usize,
}

impl Default for TaskPanel {
    fn default() -> Self {
        Self {
            focus: FocusTarget::Input,
            frame_open: false,
            window_offset: 0,
            s1_anchor: None,
            counts: crate::task_layout::TaskCounts::default(),
            unacked_failures: 0,
            popped_this_turn: false,
            user_collapsed_this_turn: false,
            view_rows: crate::task_layout::FRAME_CONTENT_ROWS,
        }
    }
}

impl App {
    /// Clamp a stale selection to the new last row (or back to the input
    /// when the panel is empty). The panel lists sub-agents and background
    /// rows (combined `task_row_count`).
    pub(crate) fn clamp_task_focus(&mut self) {
        let total = self.task_row_count();
        if let FocusTarget::TaskList(index) = &self.task_panel.focus
            && *index >= total
        {
            self.task_panel.focus = if total == 0 {
                FocusTarget::Input
            } else {
                FocusTarget::TaskList(total - 1)
            };
        }
    }

    /// Re-derive the frame's scroll window: keep the selection (or the S1
    /// anchor) visible with minimal movement; with neither, follow the tail
    /// (new work lands there — design detail 3).
    pub(crate) fn sync_task_window(&mut self) {
        let total = self.task_row_count();
        let sel = match self.task_panel.focus {
            FocusTarget::TaskList(i) => Some(i),
            FocusTarget::Input => None,
        };
        self.task_panel.window_offset = window_offset(
            self.task_panel.window_offset,
            sel,
            self.task_panel.s1_anchor,
            total,
            self.task_panel.view_rows,
        );
    }

    // ── Three-state toggle machine (S0 collapsed / S1 open-unselected /
    //    S2 open-selected) ─────────────────────────────────────────────────

    /// Ctrl+T: S0 → S2 (open + enter the list, D10 landing) or S1 when the
    /// list is empty; from S1/S2 → S0. A manual collapse outranks the
    /// failure auto-expand for the rest of the turn (F9).
    pub fn toggle_task_panel(&mut self) {
        if self.task_panel.frame_open {
            self.task_panel.user_collapsed_this_turn = true;
            self.task_panel.frame_open = false;
            self.task_panel.focus = FocusTarget::Input;
            self.task_panel.s1_anchor = None;
            // Collapse runs cleanup: rows already past their review window
            // go the moment the user stops looking (F8).
            self.cleanup_completed_agents();
        } else {
            self.task_panel.frame_open = true;
            self.enter_task_list();
        }
        self.sync_task_window();
    }

    /// Enter the list (S2) at the D10 landing: the first unacked-failure row
    /// when there is one (一步到病灶), else the last row. Entering S2 clears
    /// the `· k new` badge. An empty list lands in S1 — there is no row to
    /// select.
    fn enter_task_list(&mut self) {
        let rows = self.task_rows();
        if rows.is_empty() {
            self.task_panel.focus = FocusTarget::Input;
            self.task_panel.s1_anchor = None;
            return;
        }
        let landing = if self.task_panel.unacked_failures > 0 {
            rows.iter()
                .position(|r| self.row_is_failed(r))
                .unwrap_or(rows.len() - 1)
        } else {
            rows.len() - 1
        };
        self.task_panel.focus = FocusTarget::TaskList(landing);
        self.task_panel.s1_anchor = None;
        self.task_panel.unacked_failures = 0;
    }

    /// A panel row whose run failed (sub-agent rows only — the ✗ bucket's
    /// source is the sub-agent ledger).
    fn row_is_failed(&self, row: &TaskRow) -> bool {
        match row {
            TaskRow::SubAgent(path) => self
                .sub_agents
                .get(path)
                .is_some_and(|s| matches!(s.status, SubAgentStatus::Failed { .. })),
            TaskRow::Background(_) => false,
        }
    }

    /// ↑: at the composer (S0/S1) it opens the frame and enters the list
    /// (D10 landing); in S2 it moves the selection up (stops at row 0).
    pub fn task_list_up(&mut self) {
        match self.task_panel.focus {
            FocusTarget::Input => {
                self.task_panel.frame_open = true;
                self.enter_task_list();
            }
            FocusTarget::TaskList(i) => {
                self.task_panel.focus = FocusTarget::TaskList(i.saturating_sub(1));
            }
        }
        self.sync_task_window();
    }

    /// ↓ (S2 only — otherwise the key falls through to the composer): one
    /// row down, or back to S1 when already on the last row.
    pub fn task_list_down(&mut self) {
        if let FocusTarget::TaskList(i) = self.task_panel.focus {
            let total = self.task_row_count();
            self.task_panel.focus = if i + 1 >= total {
                FocusTarget::Input // S2 → S1 at the list end
            } else {
                FocusTarget::TaskList(i + 1)
            };
            self.sync_task_window();
        }
    }

    /// Home/End (S2 only): jump the selection to the first/last row.
    pub fn task_list_home_end(&mut self, home: bool) {
        let total = self.task_row_count();
        if total > 0 && matches!(self.task_panel.focus, FocusTarget::TaskList(_)) {
            self.task_panel.focus = FocusTarget::TaskList(if home { 0 } else { total - 1 });
            self.sync_task_window();
        }
    }

    /// PgUp/PgDn (S2 only): move the selection by one window page; the
    /// window follows.
    pub fn task_list_page(&mut self, up: bool) {
        if let FocusTarget::TaskList(i) = self.task_panel.focus {
            let total = self.task_row_count();
            let page = self.task_panel.view_rows.max(1);
            let next = if up {
                i.saturating_sub(page)
            } else {
                (i + page).min(total.saturating_sub(1))
            };
            self.task_panel.focus = FocusTarget::TaskList(next);
            self.sync_task_window();
        }
    }

    /// Esc while in S2: drop back to S1 — the composer draft is untouched
    /// (D8) and the second Esc falls through to normal Esc semantics.
    /// Returns true when the key was consumed.
    pub fn task_list_escape(&mut self) -> bool {
        if matches!(self.task_panel.focus, FocusTarget::TaskList(_)) {
            self.task_panel.focus = FocusTarget::Input;
            // S1 with no anchor = the window snaps to the tail (S5).
            self.task_panel.s1_anchor = None;
            self.sync_task_window();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
#[path = "task_focus_tests.rs"]
mod task_focus_tests;
