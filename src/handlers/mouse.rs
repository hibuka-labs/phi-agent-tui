//! Mouse event handling for the TUI application.
//!
//! Character-precise transcript selection: a left-drag selects exactly the
//! cells dragged over (a wide CJK character is always whole), a double click
//! selects the word run under the cursor, a triple click selects the whole
//! logical line, and a drag held at the output pane's top/bottom edge keeps
//! scrolling. The right-click menu copies the selection. All the selection
//! geometry lives in [`phi_tui::selection`]; this module only maps screen
//! events onto it.

use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEventKind};
use phi_tui::selection::{CharPoint, MenuClick, PressKind, snap_start, word_run};

use crate::app::{Action, App, ContextMenu};

/// How often a drag pinned to a pane edge scrolls one row.
pub(crate) const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(100);

/// Which output-pane edge the pointer hugs during a drag (edge auto-scroll).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DragEdge {
    Top,
    Bottom,
}

/// Left-drag state across mouse events: whether the button is down, which
/// edge row the pointer hugs (arming auto-scroll), the pointer's last column
/// (which column the head extends at after a scroll), and the press point a
/// multi-click selection must re-anchor at on the first drag — dragging right
/// after a double/triple click starts a fresh selection from the press point,
/// not from the word/line pick.
#[derive(Debug, Default)]
pub(crate) struct DragState {
    pub(crate) active: bool,
    pub(crate) edge: Option<DragEdge>,
    pub(crate) last_x: u16,
    pub(crate) reanchor_at: Option<CharPoint>,
    pub(crate) last_step: Option<Instant>,
}

impl App {
    /// Handle a mouse event at `(x, y)` (terminal cells) against a terminal of
    /// `(area_w, area_h)` cells. Left-press classifies the click (single /
    /// double / triple via the click chain) and anchors or picks a selection;
    /// left-drag extends it (re-anchoring first when a pick just happened);
    /// right-press opens the copy menu. While the copy menu is open, a click
    /// inside its popup activates that item: the copy row copies the
    /// selection, the dismiss row closes, and `Action::CopySelection` is
    /// returned for the caller to run (mirrors the keyboard Enter path).
    pub(crate) fn handle_mouse(
        &mut self,
        kind: MouseEventKind,
        x: u16,
        y: u16,
        area_w: u16,
        area_h: u16,
    ) -> Option<Action> {
        use MouseEventKind::*;

        // Menu is open: a click (either button) inside the drawn popup activates
        // the item under the cursor instead of falling through to selection.
        if let Some(click) = self.selection_state.menu_click(kind, x, y, area_w, area_h) {
            return match click {
                MenuClick::Copy => Some(Action::CopySelection),
                MenuClick::Dismiss => None,
            };
        }

        match kind {
            Down(MouseButton::Left) => {
                let pt = self.char_point_at(x, y);
                let press = self.selection_state.press(pt, Instant::now());
                self.drag = DragState {
                    active: true,
                    edge: self.edge_at(y),
                    last_x: x,
                    reanchor_at: None,
                    last_step: None,
                };
                match press {
                    PressKind::Single => self.selection_state.anchor(pt),
                    PressKind::Double | PressKind::Triple => {
                        self.select_click_unit(pt, press);
                    }
                }
                None
            }
            Drag(MouseButton::Left) => {
                if !self.drag.active {
                    // A drag whose press was lost (focus glitch): treat it as
                    // a fresh press so the gesture still selects something.
                    let pt = self.char_point_at(x, y);
                    self.selection_state.press(pt, Instant::now());
                    self.selection_state.anchor(pt);
                    self.drag.active = true;
                }
                self.drag.last_x = x;
                self.drag.edge = self.edge_at(y);
                if let Some(p) = self.drag.reanchor_at.take() {
                    // Drag after a word/line pick: the pick is replaced by a
                    // character-precise selection anchored at the press point.
                    self.selection_state.reset_clicks();
                    self.selection_state.anchor(Some(p));
                }
                self.selection_state.extend(self.char_point_at(x, y));
                None
            }
            Up(MouseButton::Left) => {
                self.drag = DragState::default();
                None
            }
            Down(MouseButton::Right) => {
                self.selection_state.open_menu(x, y);
                None
            }
            _ => None,
        }
    }

    /// The double/triple-click selection unit at `pt`: the word run under the
    /// cursor (double) or the whole logical line's visual block (triple).
    fn select_click_unit(&mut self, pt: Option<CharPoint>, press: PressKind) {
        let Some(p) = pt else {
            self.selection_state.anchor(None);
            return;
        };
        match press {
            PressKind::Double => {
                let Some(row_text) = self.row_plain_text(p.row).map(str::to_string) else {
                    return;
                };
                let (lo, hi) = word_run(&row_text, p.col);
                self.selection_state.select_range(
                    CharPoint {
                        row: p.row,
                        col: lo,
                    },
                    CharPoint {
                        row: p.row,
                        col: hi.saturating_sub(1),
                    },
                );
            }
            PressKind::Triple => {
                let (top, bottom) = self
                    .visual_map
                    .block_bounds(p.row)
                    .unwrap_or((p.row, p.row));
                self.selection_state.select_range(
                    CharPoint { row: top, col: 0 },
                    CharPoint {
                        row: bottom,
                        col: usize::MAX,
                    },
                );
            }
            PressKind::Single => {}
        }
        self.drag.reanchor_at = Some(p);
    }

    /// One self-paced step of edge auto-scroll, called every input drain: a
    /// left-drag pinned to the output pane's top/bottom edge row scrolls one
    /// row per [`AUTOSCROLL_INTERVAL`] and extends the selection head onto
    /// the newly revealed edge row at the pointer's column. Returns `true`
    /// when the view moved (the caller redraws).
    pub(crate) fn edge_autoscroll_step(&mut self, now: Instant) -> bool {
        let (edge, x) = match (self.drag.active, self.drag.edge) {
            (true, Some(edge)) => (edge, self.drag.last_x),
            _ => return false,
        };
        if self
            .drag
            .last_step
            .is_some_and(|t| now.saturating_duration_since(t) < AUTOSCROLL_INTERVAL)
        {
            return false;
        }
        self.drag.last_step = Some(now);
        let Some((_, _, _, ah)) = self.output_area else {
            return false;
        };
        let moved = match edge {
            DragEdge::Top => self.viewport.scroll_up(1),
            DragEdge::Bottom => self.viewport.scroll_down(1),
        };
        if !moved {
            return false;
        }
        let window = self.viewport.window_range(self.row_total(), ah as usize);
        let row = match edge {
            DragEdge::Top => window.start,
            DragEdge::Bottom => window.end.saturating_sub(1),
        };
        let pt = self.char_point_in_row(row, x);
        self.selection_state.extend(pt);
        true
    }

    /// Map a screen cell to a character point (visual row + display column,
    /// snapped to the character's first cell), or `None` when outside the
    /// output pane or over a still-streaming (uncommitted) line.
    fn char_point_at(&self, x: u16, y: u16) -> Option<CharPoint> {
        let row = self.line_index_at(x, y)?;
        self.char_point_in_row(row, x)
    }

    /// The character point at visual `row` and screen column `x` (snapped to
    /// the character's first cell).
    fn char_point_in_row(&self, row: usize, x: u16) -> Option<CharPoint> {
        let (ax, ..) = self.output_area?;
        let col = x.saturating_sub(ax) as usize;
        let text = self.row_plain_text(row).unwrap_or("");
        Some(CharPoint {
            row,
            col: snap_start(text, col),
        })
    }

    /// Plain display text of visual row `row` (the visual map after the first
    /// render, raw transcript rows before it — the same fallback
    /// [`Self::line_index_at`] hit-tests against).
    fn row_plain_text(&self, row: usize) -> Option<&str> {
        if self.visual_map.is_empty() {
            self.transcript.output.get(row).map(|l| l.text.as_str())
        } else {
            self.visual_map.texts().get(row).map(String::as_str)
        }
    }

    /// Which pane edge row `y` sits on, if any (edge auto-scroll arming).
    fn edge_at(&self, y: u16) -> Option<DragEdge> {
        let (_, ay, _, ah) = self.output_area?;
        if y == ay {
            Some(DragEdge::Top)
        } else if y + 1 == ay + ah {
            Some(DragEdge::Bottom)
        } else {
            None
        }
    }

    /// Row count the selection is clamped against (visual rows after the
    /// first render, raw output rows before it).
    pub(crate) fn row_total(&self) -> usize {
        if self.visual_map.is_empty() {
            self.transcript.len()
        } else {
            self.viewport.rendered_total
        }
    }

    /// Map a screen cell `(x, y)` to an `output` line index, or `None` when
    /// outside the output pane or over a still-streaming (uncommitted) line.
    fn line_index_at(&self, x: u16, y: u16) -> Option<usize> {
        let (ax, ay, aw, ah) = self.output_area?;
        if x < ax || x >= ax.saturating_add(aw) || y < ay || y >= ay.saturating_add(ah) {
            return None;
        }
        let row = (y - ay) as usize;
        let committed = self.transcript.len();

        // Use the visual map when available (after render), otherwise
        // fall back to direct output indices (for tests or before first render).
        if self.visual_map.is_empty() {
            // Fallback: no markdown expansion, output lines map 1:1 to visual lines.
            let tail = self
                .streaming_tail_lines()
                .map(|(l, _)| l.len())
                .unwrap_or(0);
            let window = self.viewport.window_range(committed + tail, ah as usize);
            let idx = window.start + row;
            tracing::debug!(
                x,
                y,
                row,
                committed,
                tail,
                window_start = window.start,
                window_end = window.end,
                idx,
                "line_index_at (fallback, no mapping)"
            );
            if idx < window.end && idx < committed {
                return Some(idx);
            }
            return None;
        }

        let total = self.viewport.rendered_total;
        let window = self.viewport.window_range(total, ah as usize);
        let visual_idx = window.start + row;
        let out_idx = self.visual_map.output_at(visual_idx);
        tracing::debug!(
            x,
            y,
            row,
            total,
            committed,
            window_start = window.start,
            window_end = window.end,
            visual_idx,
            out_idx,
            mapping_len = self.visual_map.len(),
            "line_index_at"
        );
        if visual_idx < window.end
            && let Some(mapped) = self.visual_map.output_at(visual_idx)
            && mapped < committed
        {
            return Some(visual_idx); // return visual index, not output index
        }
        None
    }

    /// True when visual line `i` falls inside the active selection.
    pub fn is_selected(&self, i: usize) -> bool {
        self.selection_state.is_selected(i, self.row_total())
    }

    /// The cell-column interval to highlight on visual row `i` (the row's
    /// plain display text), or `None` when the row is not selected. Half-open
    /// in display cells, wide characters whole; `hi` may be [`usize::MAX`]
    /// (to the row's end). See [`phi_tui::selection::SelectionState::selected_cols`].
    pub fn selected_cols(&self, i: usize, row_text: &str) -> Option<(usize, usize)> {
        self.selection_state
            .selected_cols(i, self.row_total(), row_text)
    }

    /// The selected text joined as plain text (what-you-see-is-what-you-copy).
    /// Uses the visual map's row texts and copy seams so the copy matches
    /// exactly what the user selected visually.
    pub fn selection_text(&self) -> String {
        let fallback: Vec<&str> = self
            .transcript
            .output
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        self.selection_state.text(&fallback, &self.visual_map)
    }

    /// Clear the active transcript selection.
    pub fn clear_selection(&mut self) {
        self.selection_state.clear();
    }

    /// The right-click menu, if open.
    pub fn context_menu(&self) -> Option<&ContextMenu> {
        self.selection_state.context_menu()
    }
}
