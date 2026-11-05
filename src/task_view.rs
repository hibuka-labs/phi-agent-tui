//! Render-ready row views for the task face (Rev 2026-11-04: live rows
//! replace the count strip).
//!
//! The middle layer of the task-row pipeline: `task_panel.rs` keeps the
//! lifecycle bookkeeping and the display set, `render.rs` maps these views to
//! theme colors. This module assembles one displayable row per display-set
//! entry and trims the frameless live face to its hard 6-row cap. Data in,
//! data out — no terminal, no mutation.

use std::time::{Duration, Instant};

use phi_kernel_tools::background_shell::BackgroundTaskStatus;

use crate::app::{App, SubAgentStatus, is_writing_hint};
use crate::task_layout::{self, RowTint, activity_cell, bg_glyph, bg_status_word, status_glyph};
use crate::task_panel::TaskRow;

/// What the activity cell is saying — render maps each to its own color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    /// Live tool feed, tool still running (`> tool`).
    LiveTool,
    /// Live tool feed, tool just finished (`+ tool`).
    LiveToolDone,
    /// `writing...` draft hint (args still streaming in).
    Writing,
    /// Terminal verdict (`done 2m13s` / `FAILED (...)` / `cancelled`).
    Verdict,
}

/// One displayable row — both faces render from this (Rev 2026-11-04: the
/// frameless live face is the box's row rendering with columns stripped).
#[derive(Debug, Clone)]
pub(crate) struct TaskRowView {
    /// Glyph table entry (shape), paired with `tint` and the activity text.
    pub(crate) glyph: &'static str,
    pub(crate) tint: RowTint,
    pub(crate) name: String,
    /// Files column (`2 files`); the deadline fuse (`t=2m` / `daemon`) for
    /// background rows. Stripped on the live face.
    pub(crate) files: String,
    pub(crate) activity: String,
    pub(crate) activity_kind: ActivityKind,
    /// FAILED summaries elide Tail — the reason lives at the tail.
    pub(crate) activity_tail_elide: bool,
    /// Dim corpse: a touched row that landed terminal after being walked
    /// away from. Stays until a death sweep.
    pub(crate) corpse: bool,
    /// Unacked-failure remnant (red until the frame opens / row is seen).
    pub(crate) remnant: bool,
    pub(crate) selected: bool,
}

impl App {
    /// Display rows as render-ready views (both faces share these).
    pub(crate) fn task_row_views(&self) -> Vec<TaskRowView> {
        let now = Instant::now();
        self.task_rows()
            .into_iter()
            .filter_map(|row| self.row_view(row, now))
            .collect()
    }

    /// Assemble one row's view. `None` when the row's store entry vanished
    /// mid-frame (the display filter already checked, but borrows are
    /// fallible like everything else).
    fn row_view(&self, row: TaskRow, now: Instant) -> Option<TaskRowView> {
        let id = row.id();
        let selected = self.task_panel.is_selected(id);
        let touched = self.task_panel.touched.contains(id);
        let remnant = self.task_panel.unacked_failures.contains(id);
        let (glyph, tint, name, files, activity, activity_kind, activity_tail_elide, corpse) =
            match &row {
                TaskRow::SubAgent(path) => {
                    let state = self.sub_agents.get(path)?;
                    let (glyph, tint) = status_glyph(&state.status);
                    let elapsed = match state.completed_at {
                        Some(at) => at.duration_since(state.started_at),
                        None => state.started_at.elapsed(),
                    };
                    let (activity, activity_kind, tail_elide) =
                        if matches!(state.status, SubAgentStatus::Running) {
                            if is_writing_hint(state, now) {
                                ("writing...".to_string(), ActivityKind::Writing, false)
                            } else {
                                let live = state
                                    .events
                                    .last()
                                    .map(|e| {
                                        let mark = if e.is_finished { "+" } else { ">" };
                                        (format!("{mark} {}", e.tool_name), e.is_finished)
                                    })
                                    .unwrap_or_default();
                                let kind = if live.1 {
                                    ActivityKind::LiveToolDone
                                } else {
                                    ActivityKind::LiveTool
                                };
                                (live.0, kind, false)
                            }
                        } else {
                            let tail = matches!(state.status, SubAgentStatus::Failed { .. });
                            (
                                activity_cell(&state.status, elapsed),
                                ActivityKind::Verdict,
                                tail,
                            )
                        };
                    let corpse = touched && !matches!(state.status, SubAgentStatus::Running);
                    (
                        glyph,
                        tint,
                        state.name.clone(),
                        task_layout::files_cell(state.files.len()),
                        activity,
                        activity_kind,
                        tail_elide,
                        corpse,
                    )
                }
                TaskRow::Background(bg_id) => {
                    let task = self.background_tasks.get(bg_id)?;
                    let (glyph, tint) = bg_glyph(&task.status);
                    // Deadline column: the fuse for bounded jobs, `daemon`
                    // for servers that run until they die.
                    let fuse = if task.is_indefinite() {
                        "daemon".to_string()
                    } else {
                        format!(
                            "t={}",
                            task_layout::format_time(Duration::from_millis(task.timeout_ms))
                        )
                    };
                    let corpse = touched && task.status != BackgroundTaskStatus::Running;
                    // A running row's activity is live process text, not a
                    // verdict — the kind drives the color map in render.rs.
                    let kind = if task.status == BackgroundTaskStatus::Running {
                        ActivityKind::LiveTool
                    } else {
                        ActivityKind::Verdict
                    };
                    (
                        glyph,
                        tint,
                        task.command.clone(),
                        fuse,
                        bg_status_word(&task.status).to_string(),
                        kind,
                        false,
                        corpse,
                    )
                }
            };
        Some(TaskRowView {
            glyph,
            tint,
            name,
            files,
            activity,
            activity_kind,
            activity_tail_elide,
            corpse,
            remnant,
            selected,
        })
    }
}

/// The live face's shown rows plus its two hidden counts (Rev 2026-11-04:
/// past the 6-row cap, **corpses go first**, then the running tail). The
/// trim passes run in that order, tail→head within each (the head of the
/// list is where the eye lands); the pinned row is never trimmed, and a
/// failure remnant only goes once nothing else is left (the physics floor —
/// more than 6 unacked failures would otherwise blow the hard cap; the frame
/// still lists everything). Returns `(shown, hidden_more, hidden_done)` where
/// `hidden_done` counts trimmed rows already at a verdict.
pub(crate) fn live_face_split(views: &[TaskRowView]) -> (Vec<TaskRowView>, usize, usize) {
    const CAP: usize = task_layout::LIVE_FACE_ROWS;
    let mut keep = vec![true; views.len()];
    let mut hidden_more = 0usize;
    let mut hidden_done = 0usize;
    let over = |keep: &[bool]| keep.iter().filter(|k| **k).count() > CAP;
    for pass in 0..3 {
        while over(&keep) {
            let victim = views.iter().enumerate().rev().find(|(i, v)| {
                keep[*i]
                    && !v.selected
                    && match pass {
                        0 => v.corpse && !v.remnant,
                        1 => !v.corpse && !v.remnant,
                        _ => true,
                    }
            });
            let Some((i, _)) = victim else { break };
            keep[i] = false;
            if views[i].activity_kind == ActivityKind::Verdict {
                hidden_done += 1;
            } else {
                hidden_more += 1;
            }
        }
    }
    let shown = views
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(v, _)| v.clone())
        .collect();
    (shown, hidden_more, hidden_done)
}

#[cfg(test)]
#[path = "task_view_tests.rs"]
mod task_view_tests;
