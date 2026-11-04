//! Pure layout and wording for the task panel and the process-row task
//! segment (design 20261103: 字符串表 / glyph 表 / 挤压梯 / FAILED 条件格式).
//!
//! Data in, data out: no terminal, no `App` state. `render.rs` maps the
//! returned faces to theme colors, `app.rs` folds the returned strings into
//! the status line, `task_panel.rs` keeps the counters. Everything the design
//! doc pinned as「照抄」lives here so the wording has exactly one home.

use std::time::Duration;

use phi_tui::wrap::{Elide, elide, pad_cols};
use unicode_width::UnicodeWidthStr;

/// A row's lifecycle state — the display vocabulary shared by the glyph
/// table, the counts ledger and the ledger markers. `Failed` carries the
/// error summary verbatim (the run's only failure fact — `RunOutcome` has no
/// exit-code field, so nothing here ever invents one).
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
/// detail 6): grep-able after the row is reaped. `Running` is not a terminal
/// and has no ledger line — callers finalize first.
pub fn ledger_line(path: &str, status: &SubAgentStatus, elapsed: Duration) -> String {
    match status {
        SubAgentStatus::Running => format!("* [{path}] started"),
        SubAgentStatus::Done => format!("* [{path}] done ({})", format_time(elapsed)),
        SubAgentStatus::Failed { summary } => format!("* [{path}] {}", failed_label(summary)),
        SubAgentStatus::Cancelled => format!("* [{path}] CANCELLED"),
    }
}

/// What a root-ledger terminal marker records — the resume recount's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerMark {
    /// `* [id] done (…)`.
    Done,
    /// `* [id] FAILED (…)`.
    Failed,
    /// `* [id] CANCELLED`.
    Cancelled,
}

/// Recognize a terminal ledger line written by [`ledger_line`]. The
/// `started` line and everything else read as `None` — a count may only come
/// from a terminal marker (对账式: ✓+✗+— = 终态标记条数, 不含 started).
pub fn parse_ledger_marker(line: &str) -> Option<LedgerMark> {
    let rest = line.strip_prefix("* [")?;
    let close = rest.find(']')?;
    let after = rest[close + 1..].strip_prefix(' ')?;
    // Match the verdict word only — done lines carry ` (2m13s)`, FAILED
    // lines carry ` (…)` or nothing.
    match after.split(' ').next().unwrap_or(after) {
        "done" => Some(LedgerMark::Done),
        "FAILED" => Some(LedgerMark::Failed),
        "CANCELLED" => Some(LedgerMark::Cancelled),
        _ => None,
    }
}

/// Session-cumulative terminal counts (design detail 4: the ledger markers
/// are the source; rows may be reaped, these numbers never drop).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskCounts {
    /// `✓` — finished clean.
    pub done: usize,
    /// `✗` — failed (timeouts included: they are failures).
    pub failed: usize,
    /// `—` — cancelled (its own bucket, never red).
    pub cancelled: usize,
}

impl TaskCounts {
    /// Terminal markers recorded so far.
    pub fn total(&self) -> usize {
        self.done + self.failed + self.cancelled
    }

    /// Add one terminal run to its bucket.
    pub fn record(&mut self, status: &SubAgentStatus) {
        match status {
            SubAgentStatus::Done => self.done += 1,
            SubAgentStatus::Failed { .. } => self.failed += 1,
            SubAgentStatus::Cancelled => self.cancelled += 1,
            SubAgentStatus::Running => {}
        }
    }

    /// ` · 7 total · 2 running · 3 done · 1 failed · 1 cancelled` (string
    /// table, wide terminals). `total` counts every task seen this session
    /// (running + terminal); `cancelled` rides only when nonzero (D9 — the
    /// exception, not the rule). English wording is the product rule: the
    /// fixed chrome ships to non-CJK markets too.
    pub fn full_block(&self, running: usize) -> String {
        let cancelled = if self.cancelled > 0 {
            format!(" · {} cancelled", self.cancelled)
        } else {
            String::new()
        };
        format!(
            " · {} total · {running} running · {} done · {} failed{cancelled}",
            running + self.total(),
            self.done,
            self.failed
        )
    }

    /// ` · 7t 2r 3d 1f 1c` — squeeze stage ① (the labels shrink to letters).
    pub fn compact_block(&self, running: usize) -> String {
        let cancelled = if self.cancelled > 0 {
            format!(" {}c", self.cancelled)
        } else {
            String::new()
        };
        format!(
            " · {}t {running}r {}d {}f{cancelled}",
            running + self.total(),
            self.done,
            self.failed
        )
    }
}

/// The two-state trigger hint (string table). `short` is squeeze stage ④ —
/// the hint shrinks before it dies, and it dies last: discoverability was
/// sewn in by hand, so it is the last thing to go.
pub fn hint_block(frame_open: bool, short: bool) -> String {
    let (full, brief) = match (frame_open, short) {
        (false, _) => ("Ctrl+T tasks", "^T tasks"),
        (true, _) => ("Ctrl+T close", "^T close"),
    };
    if short {
        format!(" · {brief}")
    } else {
        format!(" · {full}")
    }
}

/// The frame's title (string table, design mock `Tasks (23) · 1 new`): the
/// live row count plus the unacked-failure badge. Lives here with the rest of
/// the string table — the `·` is a deliberate design glyph (CJK-ambiguous
/// width; every consumer measures with display columns).
pub fn panel_title(total: usize, unacked_failures: usize) -> String {
    if unacked_failures > 0 {
        format!("Tasks ({total}) · {unacked_failures} new")
    } else {
        format!("Tasks ({total})")
    }
}

/// The task segment appended to the process row (design detail 1). Mounted
/// only once a session has task history (`show`) — a session that has never
/// had a task renders its process row untouched (string table, S0 空).
#[derive(Debug, Clone, Default)]
pub struct TaskSegment {
    /// Session-cumulative counts (zeros included while everything runs).
    pub counts: TaskCounts,
    /// Live running sub-agent rows — the `running` bucket (the counts are
    /// terminal-only by the ledger rule).
    pub running: usize,
    /// Frame-open state — picks the hint's wording.
    pub frame_open: bool,
    /// Whether the segment is mounted at all (task history exists).
    pub show: bool,
}

impl TaskSegment {
    /// The counts + hint blocks at one degradation level of the squeeze
    /// ladder (`counts_compact` = stage ①, `hint`: `Some(false)` wide /
    /// `Some(true)` = stage ④ short / `None` = the hint is dead).
    fn blocks(&self, counts_compact: bool, hint: Option<bool>) -> String {
        if !self.show {
            return String::new();
        }
        let counts = if counts_compact {
            self.counts.compact_block(self.running)
        } else {
            self.counts.full_block(self.running)
        };
        match hint {
            Some(short) => format!("{}{}", counts, hint_block(self.frame_open, short)),
            None => counts,
        }
    }
}

/// The process row's lead text (spinner/phase/waiting) split at the activity
/// suffix so the squeeze ladder can trim it in stages (string table rank:
/// spinner/phase > waiting 文 > 计数 > 提示 > Tip).
#[derive(Debug, Clone)]
pub struct LeadPieces {
    /// Everything before the activity suffix (incl. the trailing `...`).
    pub head: String,
    /// The live suffix (` - 42s - 15.0k tok`), empty when idle.
    pub activity: String,
    /// Everything after the activity suffix (reassurance tail, cancel hint).
    pub tail: String,
    /// True for the waiting leads whose tail is the reassurance clause the
    /// ladder drops at stage ③b. Phase leads keep theirs at every stage.
    pub tail_is_reassurance: bool,
}

impl LeadPieces {
    /// Stage 0: the lead whole.
    pub fn full(&self) -> String {
        format!("{}{}{}", self.head, self.activity, self.tail)
    }

    /// Stage ③a: activity suffix dropped.
    pub fn no_activity(&self) -> String {
        format!("{}{}", self.head, self.tail)
    }

    /// Stage ③b: the reassurance tail dropped as well (phase leads keep it).
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
/// and whether a Tip may still ride behind it (squeeze stage ②).
#[derive(Debug, Clone)]
pub struct StatusFit {
    /// The status line to draw.
    pub text: String,
    /// `false` once the squeeze ladder has spent the Tip's room.
    pub tip_allowed: bool,
}

fn tip_room(text: &str, width: usize) -> bool {
    UnicodeWidthStr::width(text) + TIP_GAP + TIP_MIN_COLS <= width
}

fn fits(text: &str, width: usize) -> bool {
    UnicodeWidthStr::width(text) <= width
}

/// Compose + squeeze the process row for `width` columns. The degradation
/// ladder (design「窄/矮终端」): ① counts compact → ② Tip drops → ③ waiting
/// trims (activity suffix → reassurance tail) → ④ hint shrinks to `^T` →
/// the hint dies last; whatever still overflows hard-cuts from the tail
/// (rank order — the weak tenants sit at the tail).
pub fn fit_status(lead: &LeadPieces, seg: &TaskSegment, width: usize) -> StatusFit {
    // Stage 0: everything wide (the string-table wording).
    let wide = format!("{}{}", lead.full(), seg.blocks(false, Some(false)));
    if tip_room(&wide, width) {
        return StatusFit {
            text: wide,
            tip_allowed: true,
        };
    }
    // ① counts compact, full hint, tip room still reserved.
    let compact = format!("{}{}", lead.full(), seg.blocks(true, Some(false)));
    if tip_room(&compact, width) {
        return StatusFit {
            text: compact,
            tip_allowed: true,
        };
    }
    // ② the Tip is gone; the status may spend the whole width. ③ trims the
    // waiting lead (activity → reassurance tail), ④ shrinks the hint, and
    // the hint's complete disappearance is the last named sacrifice.
    let candidates = [
        compact.clone(),
        format!("{}{}", lead.no_activity(), seg.blocks(true, Some(false))),
        format!("{}{}", lead.min(), seg.blocks(true, Some(false))),
        format!("{}{}", lead.min(), seg.blocks(true, Some(true))),
        format!("{}{}", lead.min(), seg.blocks(true, None)),
    ];
    for text in candidates {
        if fits(&text, width) {
            return StatusFit {
                text,
                tip_allowed: false,
            };
        }
    }
    // Beyond the ladder: hard-cut the tail (weak tenants sit at the tail).
    let last = format!("{}{}", lead.min(), seg.blocks(true, None));
    StatusFit {
        text: elide(&last, width, Elide::Head),
        tip_allowed: false,
    }
}

/// Wide form — the exact string-table wording, for `status_line()` and tests.
pub fn compose_status(lead: &LeadPieces, seg: &TaskSegment) -> String {
    format!("{}{}", lead.full(), seg.blocks(false, Some(false)))
}

/// Content rows inside the task frame at full size (6 + 2 borders = 8).
pub const FRAME_CONTENT_ROWS: usize = 6;
/// One content row is the floor before the frame is suppressed outright.
pub const FRAME_MIN_CONTENT: usize = 1;

/// Panel height in terminal rows for `rows` live rows with `avail` rows to
/// spare: `min(rows, 6) + 2` (content + borders), content shrinking 6→1 on
/// short terminals and the frame suppressed (0) when even one content row
/// won't fit — the status line always survives (design: 矮终端).
pub fn frame_height(rows: usize, avail: u16) -> u16 {
    let content = rows.clamp(FRAME_MIN_CONTENT, FRAME_CONTENT_ROWS);
    let full = content as u16 + 2;
    if full <= avail {
        return full;
    }
    if avail >= FRAME_MIN_CONTENT as u16 + 2 {
        avail
    } else {
        0
    }
}

/// Keep the selection (or the S1 anchor) visible in a `view_rows` window over
/// `total` rows with minimal movement; with neither, anchor the tail (new
/// work lands there — design detail 3). Always clamped to a legal offset.
pub fn window_offset(
    offset: usize,
    sel: Option<usize>,
    anchor: Option<usize>,
    total: usize,
    view_rows: usize,
) -> usize {
    let view = view_rows.max(1);
    let max_off = total.saturating_sub(view);
    let mut off = offset.min(max_off);
    match sel.or(anchor).filter(|t| *t < total) {
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

/// Row column widths `(name, files, activity)` for a panel `inner_w` wide,
/// squeezed right→left (design「列收缩」): the activity column is the flex
/// column and absorbs the shrink first, then the files column, then the name
/// down to its 12-column floor. Measured in display columns.
pub fn column_widths(inner_w: usize, name_wanted: usize) -> (usize, usize, usize) {
    const NAME_MIN: usize = 12;
    const NAME_MAX: usize = 16;
    const FILES_W: usize = 8;

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
