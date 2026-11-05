//! Task-frame focus machine — the three-state toggle (design 20261103,
//! rows reworked Rev 2026-11-04: live rows replace the count strip).
//!
//! S0 collapsed ⇄ S1 open-unselected ⇄ S2 open-selected. `frame_open` is the
//! single frame-visibility truth; S1 vs S2 is `focus`. Selection lands by
//! **row id** (never index — rows vanish and indices shift). Split impl of
//! `App` (methods live here, state stays on `App`): the toggle key, the list
//! navigation keys, the D10 landing rule, ack bookkeeping and the frame's
//! scroll window. Lifecycle/sweep bookkeeping stays in `task_panel`.

use std::collections::HashSet;

use crate::app::App;
use crate::task_layout::window_offset;

/// Where the keyboard focus is (re-exported through `app` for render/tests).
/// The variant carries the row **id** — landing by identity, never by index
/// (Rev 2026-11-04: rows hide at terminal, indices shift under the cursor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusTarget {
    /// Focus is on the input box (S0 collapsed / S1 open-unselected).
    Input,
    /// S2: the frame is open and this row is selected (pinned).
    TaskList(String),
}

/// Task panel UI state — the three-state toggle machine (design 20261103):
/// S0 collapsed ⇄ S1 open-unselected ⇄ S2 open-selected. `frame_open` is the
/// single frame-visibility truth; S1 vs S2 is `focus`.
#[derive(Debug, Clone)]
pub struct TaskPanel {
    /// Current focus position (`TaskList` carries the pinned row's id).
    pub focus: FocusTarget,
    /// S0 when false; S1/S2 when true.
    pub frame_open: bool,
    /// First visible row of the frame's scroll window.
    pub window_offset: usize,
    /// Failures not yet seen from the list — the red remnant + `· k new`
    /// badge. Acked (and cleared) when the frame opens or the row is
    /// inspected (D10).
    pub unacked_failures: HashSet<String>,
    /// Rows the user has selected at least once. A touched row that goes
    /// terminal lingers as a dim corpse until a death sweep; an untouched
    /// terminal row hides the moment it lands (hide-on-complete 完成即隐).
    pub touched: HashSet<String>,
    /// Rows the frame last drew — window sizing + page steps.
    pub view_rows: usize,
}

impl Default for TaskPanel {
    fn default() -> Self {
        Self {
            focus: FocusTarget::Input,
            frame_open: false,
            window_offset: 0,
            unacked_failures: HashSet::new(),
            touched: HashSet::new(),
            view_rows: crate::task_layout::FRAME_CONTENT_ROWS,
        }
    }
}

impl TaskPanel {
    /// The pinned row's id, if any.
    pub(crate) fn selected_id(&self) -> Option<&str> {
        match &self.focus {
            FocusTarget::Input => None,
            FocusTarget::TaskList(id) => Some(id.as_str()),
        }
    }

    /// Whether `id` is the pinned row.
    pub(crate) fn is_selected(&self, id: &str) -> bool {
        self.selected_id() == Some(id)
    }

    /// Mark a row as having been selected (it earns its corpse after it
    /// lands terminal — Rev 2026-11-04: the dim corpse is limited to rows that were selected 暗尸只限被选中过的行).
    pub(crate) fn touch(&mut self, id: &str) {
        self.touched.insert(id.to_string());
    }

    /// Ack every outstanding failure (the frame opening is the user coming
    /// to look — D10). Rows fall back to their normal lifecycle: the
    /// selected one stays pinned, the rest leave the row set.
    pub(crate) fn ack_failures(&mut self) {
        self.unacked_failures.clear();
    }
}

impl App {
    /// Drop a stale selection whose row has left the display set (safety net
    /// — a pinned row is *kept* by the display filter, so this only fires on
    /// true staleness, e.g. a background row GC'd under a wake debt).
    pub(crate) fn clamp_task_focus(&mut self) {
        if let FocusTarget::TaskList(id) = &self.task_panel.focus {
            let id = id.clone();
            let alive = self.task_rows().iter().any(|r| r.id() == id);
            if !alive {
                self.task_panel.focus = FocusTarget::Input;
            }
        }
    }

    /// Re-derive the frame's scroll window: keep the selection visible with
    /// minimal movement; with none, follow the tail (new work lands there —
    /// design detail 3).
    pub(crate) fn sync_task_window(&mut self) {
        let rows = self.task_rows();
        let sel = self
            .task_panel
            .selected_id()
            .and_then(|id| rows.iter().position(|r| r.id() == id));
        self.task_panel.window_offset = window_offset(
            self.task_panel.window_offset,
            sel,
            rows.len(),
            self.task_panel.view_rows,
        );
    }

    // ── Three-state toggle machine (S0 collapsed / S1 open-unselected /
    //    S2 open-selected) ─────────────────────────────────────────────────

    /// Ctrl+T: S0 → S2 (open + enter the list, D10 landing) or S1 when the
    /// list is empty; from S1/S2 → S0. Opening acks every outstanding
    /// failure (the box is where they are listed). On an empty row set the
    /// key is a no-op — there is no face to open (框跟着事走 — the frame follows the work).
    pub fn toggle_task_panel(&mut self) {
        if self.task_panel.frame_open {
            self.task_panel.frame_open = false;
            self.task_panel.focus = FocusTarget::Input;
            // Closing walks away from the pin → it demotes to a corpse like
            // any other (its death is a sweep's job, not this key's).
        } else {
            if self.task_rows().is_empty() {
                return;
            }
            self.task_panel.frame_open = true;
            self.enter_task_list();
            self.task_panel.ack_failures();
        }
        self.clamp_task_focus();
        self.sync_task_window();
        self.auto_sweep();
    }

    /// Enter the list (S2) at the D10 landing: the first unacked-failure row
    /// when there is one (one step to the problem 一步到病灶), else the last row. An empty list lands
    /// in S1 — there is no row to select.
    fn enter_task_list(&mut self) {
        let rows = self.task_rows();
        if rows.is_empty() {
            self.task_panel.focus = FocusTarget::Input;
            return;
        }
        let landing = rows
            .iter()
            .position(|r| self.task_panel.unacked_failures.contains(r.id()))
            .unwrap_or(rows.len() - 1);
        let id = rows[landing].id().to_string();
        self.task_panel.focus = FocusTarget::TaskList(id.clone());
        self.task_panel.touch(&id);
    }

    /// ↑: at the composer (S0/S1) it opens the frame and enters the list
    /// (D10 landing); in S2 it moves the selection up (stops at row 0).
    pub fn task_list_up(&mut self) {
        let rows = self.task_rows();
        match self.task_panel.selected_id() {
            None => {
                if rows.is_empty() {
                    return;
                }
                self.task_panel.frame_open = true;
                self.enter_task_list();
                // Opening is coming to look — same ack rule as Ctrl+T (D10).
                self.task_panel.ack_failures();
            }
            Some(id) => {
                let Some(i) = rows.iter().position(|r| r.id() == id) else {
                    return;
                };
                if let Some(prev) = rows.get(i.saturating_sub(1)) {
                    let id = prev.id().to_string();
                    self.task_panel.focus = FocusTarget::TaskList(id.clone());
                    self.task_panel.touch(&id);
                }
            }
        }
        self.sync_task_window();
    }

    /// ↓ (S2 only — otherwise the key falls through to the composer): one
    /// row down, or back to S1 when already on the last row.
    pub fn task_list_down(&mut self) {
        let rows = self.task_rows();
        let Some(id) = self.task_panel.selected_id() else {
            return;
        };
        let Some(i) = rows.iter().position(|r| r.id() == id) else {
            return;
        };
        if let Some(next) = rows.get(i + 1) {
            let id = next.id().to_string();
            self.task_panel.focus = FocusTarget::TaskList(id.clone());
            self.task_panel.touch(&id);
        } else {
            // S2 → S1 at the list end (the walked-away row demotes to a
            // corpse; a death sweep may now be due).
            self.task_panel.focus = FocusTarget::Input;
            self.auto_sweep();
        }
        self.sync_task_window();
    }

    /// Home/End (S2 only): jump the selection to the first/last row.
    pub fn task_list_home_end(&mut self, home: bool) {
        let rows = self.task_rows();
        if rows.is_empty() || self.task_panel.selected_id().is_none() {
            return;
        }
        let row = if home { rows.first() } else { rows.last() };
        if let Some(row) = row {
            let id = row.id().to_string();
            self.task_panel.focus = FocusTarget::TaskList(id.clone());
            self.task_panel.touch(&id);
        }
        self.sync_task_window();
    }

    /// PgUp/PgDn (S2 only): move the selection by one window page; the
    /// window follows.
    pub fn task_list_page(&mut self, up: bool) {
        let rows = self.task_rows();
        let Some(id) = self.task_panel.selected_id() else {
            return;
        };
        let Some(i) = rows.iter().position(|r| r.id() == id) else {
            return;
        };
        let page = self.task_panel.view_rows.max(1);
        let next = if up {
            i.saturating_sub(page)
        } else {
            (i + page).min(rows.len().saturating_sub(1))
        };
        if let Some(row) = rows.get(next) {
            let id = row.id().to_string();
            self.task_panel.focus = FocusTarget::TaskList(id.clone());
            self.task_panel.touch(&id);
        }
        self.sync_task_window();
    }

    /// Esc while in S2: drop back to S1 — the walked-away row demotes to a
    /// corpse and the composer draft is untouched (D8); the second Esc falls
    /// through to normal Esc semantics. Returns true when consumed.
    pub fn task_list_escape(&mut self) -> bool {
        if self.task_panel.selected_id().is_some() {
            self.task_panel.focus = FocusTarget::Input;
            self.sync_task_window();
            self.auto_sweep();
            true
        } else {
            false
        }
    }

    /// Typing takeover (Rev 2026-11-04, 打字即接管): a printable char,
    /// Enter or paste hands the keyboard back to the composer — the reply
    /// must be readable on the root transcript. The walked-away row demotes
    /// like any other; nothing is swept here (a death sweep may already be
    /// due and `auto_sweep` covers that). Returns true when focus moved.
    pub fn steal_focus_to_composer(&mut self) -> bool {
        if self.task_panel.selected_id().is_none() {
            return false;
        }
        self.task_panel.focus = FocusTarget::Input;
        self.sync_task_window();
        self.auto_sweep();
        true
    }
}

#[cfg(test)]
#[path = "task_focus_tests.rs"]
mod task_focus_tests;
