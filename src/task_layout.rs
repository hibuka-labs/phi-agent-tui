//! Pure layout and wording for the task panel faces and the process row
//! (Rev 2026-11-04: live rows replace the count strip 活行面替代计数条).
//!
//! Data in, data out: no terminal, no `App` state. `render.rs` maps the
//! returned faces to theme colors, `task_panel.rs` keeps the lifecycle
//! bookkeeping. Everything the design doc pins as wording lives here so it
//! has exactly one home.

use std::time::Duration;

use phi_kernel_tools::background_shell::BackgroundTaskStatus;
use phi_tui::wrap::{Elide, elide, pad_cols};
use unicode_width::UnicodeWidthStr;

/// A row's lifecycle state — the display vocabulary shared by the glyph
/// table and the ledger markers. `Failed` carries the error summary verbatim
/// (the run's only failure fact — `RunOutcome` has no exit-code field, so
/// nothing here ever invents one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubAgentStatus {
    /// In flight (registry `queued`/`running` merge into this one bucket).
    Running,
    /// Finished clean.
    Done,
    /// Finished with an error; `summary` is the error text as given.
    Failed {
        /// Error text from the run outcome (may be empty).
        summary: String,
    },
    /// Cancelled — 未成功 but its own bucket (D9: the red ✗ is for real
    /// failures only; an alarm that fires for every cause is no alarm).
    Cancelled,
}

/// Where a row's glyph/text rides in the theme (render.rs maps to `Color`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowTint {
    /// Cyan — running.
    Running,
    /// Green — done.
    Done,
    /// Red — failed.
    Error,
    /// Yellow — timed out / cancelled.
    Warn,
}

/// Whether a failure summary describes a timeout (spawn.rs's
/// `"task timed out after …"` is the only producer). Distinguished when the
/// text allows: yellow `!` like `bg_status_glyph`'s `TimedOut`, never red.
pub fn is_timeout_summary(summary: &str) -> bool {
    summary.starts_with("task timed out")
}

/// The glyph table (design「glyph 表」), aligned with `bg_status_glyph`.
/// Shape never carries meaning alone — callers pair it with the activity
/// text and the tint (the accessibility triple-encoding rule).
pub fn status_glyph(status: &SubAgentStatus) -> (&'static str, RowTint) {
    match status {
        SubAgentStatus::Running => ("*", RowTint::Running),
        SubAgentStatus::Done => ("+", RowTint::Done),
        SubAgentStatus::Failed { summary } => {
            if is_timeout_summary(summary) {
                ("!", RowTint::Warn)
            } else {
                ("!", RowTint::Error)
            }
        }
        SubAgentStatus::Cancelled => ("-", RowTint::Warn),
    }
}

/// Map a watcher wire status (`"ok"` / `"error"` / `"closed"`) to the row
/// vocabulary. Unknown words carry **no fact** — the row keeps its current
/// state (never guess a terminal from a word this build does not know).
pub fn wire_status(status: &str, summary: Option<&str>) -> Option<SubAgentStatus> {
    match status {
        "ok" => Some(SubAgentStatus::Done),
        "error" => Some(SubAgentStatus::Failed {
            summary: summary.unwrap_or_default().to_string(),
        }),
        "closed" => Some(SubAgentStatus::Cancelled),
        _ => None,
    }
}

/// The glyph table for a background task row — the same shape vocabulary as
/// [`status_glyph`] (`*` running, `+` done, `!` failed/timed out, `-`
/// cancelled), so both row kinds read alike.
pub fn bg_glyph(status: &BackgroundTaskStatus) -> (&'static str, RowTint) {
    match status {
        BackgroundTaskStatus::Running => ("*", RowTint::Running),
        BackgroundTaskStatus::Done => ("+", RowTint::Done),
        BackgroundTaskStatus::TimedOut => ("!", RowTint::Warn),
        BackgroundTaskStatus::Cancelled => ("-", RowTint::Warn),
        BackgroundTaskStatus::Error(_) => ("!", RowTint::Error),
    }
}

/// One-word status for a background row's activity cell / tail title.
pub fn bg_status_word(status: &BackgroundTaskStatus) -> &'static str {
    match status {
        BackgroundTaskStatus::Running => "running",
        BackgroundTaskStatus::Done => "done",
        BackgroundTaskStatus::TimedOut => "timeout",
        BackgroundTaskStatus::Cancelled => "cancelled",
        BackgroundTaskStatus::Error(_) => "error",
    }
}

/// Display-column budget of the error summary inside `FAILED (…)`.
pub const FAILED_SUMMARY_COLS: usize = 24;

/// Scan an error string for a shell-style exit code. Only the literal forms
/// the engine actually produces count — the failure fact source is the error
/// string alone (design: 绝不编数字), so a miss means no `(exit N)` suffix.
pub fn extract_exit_code(error: &str) -> Option<i32> {
    const FORMS: [&str; 4] = ["exit code ", "exit status ", "exited with code ", "exit "];
    let lower = error.to_ascii_lowercase();
    let mut from = 0usize;
    while from <= lower.len() {
        // Earliest occurrence of any form at/after `from`.
        let mut best: Option<(usize, &'static str)> = None;
        for form in FORMS {
            if let Some(p) = lower[from..].find(form) {
                let abs = from + p;
                if best.is_none_or(|(b, _)| abs < b) {
                    best = Some((abs, form));
                }
            }
        }
        let (pos, form) = best?;
        let digits_at = pos + form.len();
        // Word boundary on the left so a fragment like "sexit " never counts.
        let left_ok = pos == 0
            || !lower[..pos]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric());
        let rest = &error[digits_at..];
        let neg = rest.starts_with('-');
        let num_start = if neg { 1 } else { 0 };
        let digits: String = rest[num_start..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if left_ok && !digits.is_empty() {
            let next = rest[num_start + digits.len()..].chars().next();
            if next.is_none_or(|c| !c.is_ascii_digit())
                && let Ok(n) = digits.parse::<i32>()
            {
                return Some(if neg { -n } else { n });
            }
            // Unparseable (absurdly long) digit run — keep scanning; a
            // later, real form may still carry the code.
        }
        from = digits_at;
    }
    None
}

/// The activity-cell / ledger form of a failed run (design detail 6, S8):
/// `FAILED (exit 1)` when the error carries a parseable exit code, else
/// `FAILED ({summary, head-elided tail-kept ≤24 cols})`. The tail is kept
/// because the tail of an error carries the reason. A summary-less failure
/// reads bare `FAILED` — never `FAILED ()` and never a fabricated number.
pub fn failed_label(summary: &str) -> String {
    if let Some(code) = extract_exit_code(summary) {
        return format!("FAILED (exit {code})");
    }
    let flat = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "FAILED".to_string();
    }
    let short = elide(&flat, FAILED_SUMMARY_COLS, Elide::Tail { sep: None });
    format!("FAILED ({short})")
}

/// `{secs}s` / `{m}m{s}s` — the panel/ledger duration form (`done (2m13s)`).
pub fn format_time(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{}s", secs / 60, secs % 60)
    }
}

/// The root transcript's permanent ledger line for a terminal run (design
/// detail 6): grep-able after the row is swept. `Running` is not a terminal
/// and has no ledger line — callers finalize first.
pub fn ledger_line(path: &str, status: &SubAgentStatus, elapsed: Duration) -> String {
    match status {
        SubAgentStatus::Running => format!("* [{path}] started"),
        SubAgentStatus::Done => format!("* [{path}] done ({})", format_time(elapsed)),
        SubAgentStatus::Failed { summary } => format!("* [{path}] {}", failed_label(summary)),
        SubAgentStatus::Cancelled => format!("* [{path}] CANCELLED"),
    }
}

/// The process row's lead text (spinner/phase/waiting) split at the activity
/// suffix so the squeeze ladder can trim it in stages (string table rank:
/// spinner/phase > waiting 文 > Tip).
#[derive(Debug, Clone)]
pub struct LeadPieces {
    /// Everything before the activity suffix (incl. the trailing `...`).
    pub head: String,
    /// The live suffix (` - 42s - 15.0k tok`), empty when idle.
    pub activity: String,
    /// Everything after the activity suffix (reassurance tail, cancel hint).
    pub tail: String,
    /// True for the waiting leads whose tail is the reassurance clause the
    /// ladder drops at its last named stage. Phase leads keep theirs at
    /// every stage.
    pub tail_is_reassurance: bool,
}

impl LeadPieces {
    /// Stage 0: the lead whole.
    pub fn full(&self) -> String {
        format!("{}{}{}", self.head, self.activity, self.tail)
    }

    /// Stage ①: activity suffix dropped.
    pub fn no_activity(&self) -> String {
        format!("{}{}", self.head, self.tail)
    }

    /// Stage ②: the reassurance tail dropped as well (phase leads keep it).
    pub fn min(&self) -> String {
        if self.tail_is_reassurance {
            self.head.clone()
        } else {
            self.no_activity()
        }
    }
}

/// Gap between the status and a riding Tip (render_process_line).
pub const TIP_GAP: usize = 4;
/// Minimum columns a Tip needs to be worth drawing.
pub const TIP_MIN_COLS: usize = 8;

/// The fitted process row: text at its widest affordable degradation level,
/// and whether a Tip may still ride behind it.
#[derive(Debug, Clone)]
pub struct StatusFit {
    /// The status line to draw.
    pub text: String,
    /// `false` once the squeeze ladder has spent the Tip's room.
    pub tip_allowed: bool,
    /// Byte offset in `text` where the lead's tail begins — the parenthetical
    /// hint (`(Ctrl+C cancel)`, `(N daemon(s))`, the auto-continue promise).
    /// `None` when the tail is not on screen (the ladder dropped it, or the
    /// final elide cut it away).
    ///
    /// The renderer splits the row here so the live phase word and the hint
    /// beside it wear different slots (#4 配色分层): the phase word takes the
    /// brand's bright warm, the parenthetical drops to the gray hint slot.
    pub tail_start: Option<usize>,
}

fn tip_room(text: &str, width: usize) -> bool {
    UnicodeWidthStr::width(text) + TIP_GAP + TIP_MIN_COLS <= width
}

fn fits(text: &str, width: usize) -> bool {
    UnicodeWidthStr::width(text) <= width
}

/// Compose + squeeze the process row for `width` columns. The lead alone
/// carries it now (Rev 2026-11-04 retired the task segment — tasks are rows,
/// not a count strip). Ladder: full (Tip may ride) → Tip drops, then waiting
/// trims (activity suffix → reassurance tail) → whatever still overflows
/// hard-cuts from the tail (weak tenants sit at the tail).
pub fn fit_lead(lead: &LeadPieces, width: usize) -> StatusFit {
    let wide = lead.full();
    if tip_room(&wide, width) {
        return StatusFit {
            text: wide,
            tip_allowed: true,
            tail_start: Some(lead.head.len() + lead.activity.len()),
        };
    }
    // The min() stage: a phase lead's cancel hint survives every stage, so its
    // split point survives too; a reassurance tail is gone by then.
    let min_tail_start = if lead.tail_is_reassurance {
        None
    } else {
        Some(lead.head.len())
    };
    for (text, tail_start) in [
        (lead.full(), Some(lead.head.len() + lead.activity.len())),
        (lead.no_activity(), Some(lead.head.len())),
        (lead.min(), min_tail_start),
    ] {
        if fits(&text, width) {
            return StatusFit {
                text,
                tip_allowed: false,
                tail_start,
            };
        }
    }
    // Past the ladder: hard-cut from the head's tail (weak tenants at the
    // tail). The tail is gone with the cut, so no split point survives.
    StatusFit {
        text: elide(&lead.min(), width, Elide::Head),
        tip_allowed: false,
        tail_start: None,
    }
}

/// The frame's title (string table, design mock `Tasks (n)`): the live row
/// count plus the unacked-failure badge (`· k new`, D10). Lives here with the
/// rest of the string table — the `·` is a deliberate design glyph (CJK-
/// ambiguous width; every consumer measures with display columns).
pub fn panel_title(total: usize, unacked_failures: usize) -> String {
    if unacked_failures > 0 {
        format!("Tasks ({total}) · {unacked_failures} new")
    } else {
        format!("Tasks ({total})")
    }
}

/// The frame's key hint — one of only two places `^T` is ever spelled (the
/// other is the live face's overflow line). Not a standing hint: it lives in
/// the frame chrome, next to the thing it opens.
pub const FRAME_CLOSE_HINT: &str = "^T close";

/// Content rows inside the task frame at full size (6 + 1 border = 7).
///
/// #3 任务面板框只留上边框: the frame draws its top rule only, so it spends
/// one border row, not two.
pub const FRAME_CONTENT_ROWS: usize = 6;
/// One content row is the floor before the frame is suppressed outright.
pub const FRAME_MIN_CONTENT: usize = 1;
/// Border rows the frame spends on its chrome (the top rule — the only one).
pub const FRAME_BORDER_ROWS: usize = 1;

/// Hard row cap of the frameless live face (Rev 2026-11-04: hard 6-row cap 限高 6 行硬).
/// Same budget as the frame's content rows — one row region, two modes.
pub const LIVE_FACE_ROWS: usize = FRAME_CONTENT_ROWS;

/// Panel height in terminal rows for `rows` live rows with `avail` rows to
/// spare: `min(rows, 6) + 2` (content + borders), content shrinking 6→1 on
/// short terminals and the frame suppressed (0) when even one content row
/// won't fit — the status line always survives (design: 矮终端).
pub fn frame_height(rows: usize, avail: u16) -> u16 {
    let content = rows.clamp(FRAME_MIN_CONTENT, FRAME_CONTENT_ROWS);
    let full = content as u16 + FRAME_BORDER_ROWS as u16;
    if full <= avail {
        return full;
    }
    if avail >= FRAME_MIN_CONTENT as u16 + FRAME_BORDER_ROWS as u16 {
        avail
    } else {
        0
    }
}

/// Frameless live-face height for `shown` rows already trimmed to the cap,
/// with `overflow` riding under them when rows were trimmed away. Shrinks on
/// short terminals like the frame does; suppressed (0) below one row.
pub fn live_face_height(shown: usize, overflow: bool, avail: u16) -> u16 {
    let want = shown.min(LIVE_FACE_ROWS) as u16 + u16::from(overflow);
    if want <= avail {
        return want;
    }
    if avail >= 1 { avail } else { 0 }
}

/// Keep the selection visible in a `view_rows` window over `total` rows with
/// minimal movement; with no selection, anchor the tail (new work lands
/// there — design detail 3). Always clamped to a legal offset.
pub fn window_offset(offset: usize, sel: Option<usize>, total: usize, view_rows: usize) -> usize {
    let view = view_rows.max(1);
    let max_off = total.saturating_sub(view);
    let mut off = offset.min(max_off);
    match sel.filter(|t| *t < total) {
        Some(t) => {
            if t < off {
                off = t;
            } else if t >= off + view {
                off = t + 1 - view;
            }
        }
        None => off = max_off,
    }
    off.min(max_off)
}

/// Whitespace between the row's columns, in display columns. The columns are
/// held apart by this gap and their own padding alone — no rule characters
/// (规则线不入列). The gap shares what the fixed columns leave over with the
/// activity column 1:1:2, so a wide panel spreads its three columns across
/// the width instead of packing them at the left edge (列随框走); floored at
/// 2 so a narrow panel still separates its cells.
pub fn column_gap(inner_w: usize, name_wanted: usize) -> usize {
    let name_w = name_wanted.clamp(NAME_MIN, NAME_MAX);
    // marker+" " + glyph+" " + name + files = everything but the two gaps
    // and the flex activity column.
    let slack = inner_w.saturating_sub(4 + name_w + FILES_W);
    (slack / 4).max(2)
}

/// Fixed column floors/caps (display columns) — #13 任务列宽: the name is
/// the row's identity (12–28), the files column is 12.
const NAME_MIN: usize = 12;
const NAME_MAX: usize = 28;
const FILES_W: usize = 12;

/// Row column widths `(name, files, activity)` for a panel `inner_w` wide,
/// squeezed right→left (design「列收缩」): the activity column is the flex
/// column and absorbs the shrink first, then the files column, then the name
/// down to its 12-column floor. Measured in display columns.
pub fn column_widths(inner_w: usize, name_wanted: usize) -> (usize, usize, usize) {
    // The squeeze order is unchanged — activity flexes first, then files,
    // then the name.
    let mut name_w = name_wanted.clamp(NAME_MIN, NAME_MAX);
    let mut files_w = FILES_W;
    // Activity flexes first (it is the rightmost column).
    let mut act_w = inner_w.saturating_sub(name_w + files_w);
    if act_w == 0 && name_w + files_w > inner_w {
        // Then the files column, to zero.
        files_w = inner_w.saturating_sub(name_w);
    }
    if files_w == 0 && name_w > inner_w {
        // Then the name — floor 12, but physics wins on tiny panels.
        name_w = inner_w.max(NAME_MIN.min(inner_w));
        act_w = 0;
    }
    (name_w, files_w, act_w)
}

/// Frameless live-face widths `(name, activity)` — the box's columns with the
/// files column stripped (Rev 2026-11-04: the row render reuses the frame's row render 剥列).
pub fn live_row_widths(inner_w: usize, name_wanted: usize) -> (usize, usize) {
    // #13 任务列宽: the live face shares the frame's name width.
    let mut name_w = name_wanted.clamp(NAME_MIN, NAME_MAX);
    let mut act_w = inner_w.saturating_sub(name_w);
    if name_w > inner_w {
        name_w = inner_w;
        act_w = 0;
    }
    (name_w, act_w)
}

/// The live face's overflow line (Rev 2026-11-04 mock `+ 3 more · + 2 done ·
/// ^T tasks`): hidden-row counts after the 6-row trim, then the only key
/// hint outside the frame. Parts degrade away as width tightens — the hint
/// first (weakest tenant, it is discoverability, not data), then the done
/// count. `None` when nothing is hidden (the line exists for overflow only).
pub fn overflow_line(hidden_more: usize, hidden_done: usize, width: usize) -> Option<String> {
    if hidden_more == 0 && hidden_done == 0 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if hidden_more > 0 {
        parts.push(format!("+ {hidden_more} more"));
    }
    if hidden_done > 0 {
        parts.push(format!("+ {hidden_done} done"));
    }
    parts.push("^T tasks".to_string());
    // Degrade by dropping tail parts — the hint first (weakest tenant), then
    // the done count; the `more` count is the strongest data.
    for keep in (1..=parts.len()).rev() {
        let line = parts[..keep].join(" · ");
        if fits(&line, width) {
            return Some(line);
        }
    }
    // Physics: whatever data remains, hard-cut from the tail.
    let data = parts[..parts.len() - 1].join(" · ");
    Some(elide(&data, width, Elide::Head))
}

/// The files cell: `1 file` / `2 files` (mockup form; a count, not a list —
/// the child's detail view carries the names).
pub fn files_cell(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// The activity cell for a terminal sub-agent row (mockup column): the
/// verdict word plus, for done runs, their runtime (`done 2m13s`).
pub fn activity_cell(status: &SubAgentStatus, elapsed: Duration) -> String {
    match status {
        SubAgentStatus::Running => String::new(),
        SubAgentStatus::Done => format!("done {}", format_time(elapsed)),
        SubAgentStatus::Failed { summary } => failed_label(summary),
        SubAgentStatus::Cancelled => "cancelled".to_string(),
    }
}

/// Elide + right-pad one cell to `cols` display columns (the panel's column
/// contract: every cell fits its column, CJK included).
pub fn cell(text: &str, cols: usize, mode: Elide) -> String {
    pad_cols(&elide(text, cols, mode), cols)
}

#[cfg(test)]
#[path = "task_layout_tests.rs"]
mod task_layout_tests;
