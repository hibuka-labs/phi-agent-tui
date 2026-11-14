//! ratatui frame rendering: transcript (output) / composer / status bar.

mod faces;
mod frame;
mod lines;
mod output;
mod popups;
mod snapshot;

pub use frame::draw;
pub use snapshot::{buffer_to_text, snapshot_process_line_colors, snapshot_text};

// Intra-module plumbing: the clusters and the render tests reach every
// helper through `use super::*`, exactly as when this was one file.
#[allow(unused_imports)]
pub(super) use {faces::*, frame::*, lines::*, output::*, popups::*, snapshot::*};

use phi_agent::{ApprovalDecision, RiskLevel};

use ratatui::{
    Frame, Terminal,
    backend::TestBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap},
};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    AgentStatus, App, CONTEXT_MENU_H, CONTEXT_MENU_W, FocusTarget, Phase, ResultTier,
    context_menu_pos, fmt_k,
};

use crate::task_layout::{self, RowTint, format_time};

use crate::task_panel::{TaskRow, ThinkingPanelState};

use crate::task_view::{ActivityKind, TaskRowView, live_face_split};

use crate::theme::{ShellToken, SpanSpec, Theme};

use phi_tui::completer::{MentionCompleter, SlashCompleter};

use phi_tui::layout::{Preview, folded_body, opens_block};

use phi_tui::lines::{LineKind, ToolState};

use phi_tui::markdown::{line_plain_text, render_markdown_with_width};

use phi_tui::popup_list::{GUTTER_W, PopupList, band_width};

use phi_tui::wrap::{Elide, elide, pad_cols, wrap, wrap_line, wrap_line_rows, wrap_rows};

/// Max composer rows shown (its box grows with the buffer up to this).
const MAX_COMPOSER_ROWS: usize = 8;

/// Thinking panel total height (border 2 + 6 content lines).
const THINKING_PANEL_H: u16 = 8;

/// History rows that must survive below the top of the output pane for the
/// panel to show — thinking must never occlude the whole transcript.
const MIN_PANEL_HISTORY_ROWS: u16 = 2;

/// Below this terminal height the thinking panel hides entirely. Belt and
/// suspenders: at current composer bounds the fit guard already hides it
/// (term < 17 ⇒ output < 10), but a future composer/layout change must not
/// resurrect the squeeze this guard exists for.
const MIN_PANEL_TERM_H: u16 = 12;

/// User-message fold (#8): a long message (pasted log, file) shows its first
/// `USER_FOLD_HEAD` rows, a "... +N lines hidden" marker and its last
/// `USER_FOLD_TAIL` rows. The fold only engages past `USER_FOLD_THRESHOLD`
/// rows — where the marker actually saves space.
const USER_FOLD_HEAD: usize = 8;
const USER_FOLD_TAIL: usize = 2;
const USER_FOLD_THRESHOLD: usize = 12;

#[cfg(test)]
#[path = "../render_tests/mod.rs"]
mod render_tests;
