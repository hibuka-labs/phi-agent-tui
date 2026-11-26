//! Task row faces (live/frame/background) and row layout helpers.

use super::*;

/// Render the task face: one row region, two modes (Rev 2026-11-04). The
/// framed panel when open, else the frameless live face — both draw from the
/// same row views; the live face is the box's row rendering with the files
/// column stripped (column strip 剥列).
pub(super) fn render_task_face(f: &mut Frame, app: &mut App, area: Rect) {
    if app.task_panel.frame_open {
        render_frame_face(f, app, area);
    } else {
        render_live_face(f, app, area);
    }
}

/// The frameless live face: display rows hard-capped at 6 (corpses trimmed
/// first, then the running tail — [`live_face_split`]) with the overflow
/// line under them. No borders, no selection marker: when this face is up,
/// the focus is at the composer (框跟着事走 — the frame follows the work).
pub(super) fn render_live_face(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme();
    let views = app.task_row_views();
    let (shown, hidden_more, hidden_done) = live_face_split(&views);
    let inner_w = area.width as usize;

    // name 12–16 + flex activity; the files column is stripped.
    let name_wanted = views
        .iter()
        .map(|v| UnicodeWidthStr::width(v.name.as_str()))
        .max()
        .unwrap_or(0);
    let gap = task_layout::column_gap(inner_w, name_wanted);
    // marker+" ", glyph+" ", one gap between name and activity.
    let cols_budget = inner_w.saturating_sub(4 + gap);
    let (name_w, act_w) = task_layout::live_row_widths(cols_budget, name_wanted);

    let mut lines: Vec<Line> = Vec::with_capacity(shown.len() + 1);
    for v in &shown {
        lines.push(task_row_line(v, name_w, None, gap, act_w, &theme));
    }
    if let Some(overflow) = task_layout::overflow_line(hidden_more, hidden_done, inner_w) {
        lines.push(Line::from(Span::styled(
            overflow,
            Style::default().fg(theme.hint),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// One row's spans — shared by both faces (the live face passes
/// `files_w: None`, which drops the files column and its gap). Every cell is
/// elided to its column then padded OUT to it, in display columns:
/// `format!("{:<w$}")` pads by chars and would overshoot. The columns stand
/// apart on `gap` whitespace plus their own padding — no rule characters
/// between them (规则线不入列). A corpse dims; a failure remnant rides its
/// tint (the strongest fact on the row); the pinned row takes the selection
/// background.
pub(super) fn task_row_line(
    v: &TaskRowView,
    name_w: usize,
    files_w: Option<usize>,
    gap: usize,
    act_w: usize,
    theme: &Theme,
) -> Line<'static> {
    let bg = if v.selected {
        theme.selection_bg
    } else {
        Color::Reset
    };
    // Dim text must survive sitting on the selection background.
    let faint = if v.selected {
        theme.lifted
    } else {
        theme.faint
    };
    let dim = v.corpse && !v.remnant;
    let tint = tint_color(v.tint);
    let glyph_color = if dim { faint } else { tint };
    let name_color = if dim {
        faint
    } else if v.remnant {
        tint
    } else {
        Color::Reset
    };
    let act_color = if dim {
        faint
    } else {
        match v.activity_kind {
            ActivityKind::LiveTool => Color::Reset,
            ActivityKind::LiveToolDone | ActivityKind::Writing => faint,
            ActivityKind::Verdict => match v.tint {
                RowTint::Error => Color::Red,
                RowTint::Warn => Color::Yellow,
                _ => faint,
            },
        }
    };
    let act_mode = if v.activity_tail_elide {
        Elide::Tail { sep: None }
    } else {
        Elide::Head
    };

    let mut spans = Vec::with_capacity(5);
    spans.push(Span::styled(
        if v.selected { "> " } else { "  " }.to_string(),
        Style::default().bg(bg),
    ));
    spans.push(Span::styled(
        format!("{} ", v.glyph),
        Style::default().fg(glyph_color).bg(bg),
    ));
    spans.push(Span::styled(
        task_layout::cell(&v.name, name_w, Elide::Head),
        Style::default().fg(name_color).bg(bg),
    ));
    if let Some(files_w) = files_w {
        spans.push(Span::styled(
            format!(
                "{}{}",
                " ".repeat(gap),
                task_layout::cell(&v.files, files_w, Elide::Head)
            ),
            Style::default().fg(faint).bg(bg),
        ));
        spans.push(Span::styled(
            format!(
                "{}{}",
                " ".repeat(gap),
                task_layout::cell(&v.activity, act_w, act_mode)
            ),
            Style::default().fg(act_color).bg(bg),
        ));
    } else {
        // Live face: no separator — the same gap after the padded name column.
        spans.push(Span::styled(" ".repeat(gap), Style::default().bg(bg)));
        spans.push(Span::styled(
            task_layout::cell(&v.activity, act_w, act_mode),
            Style::default().fg(act_color).bg(bg),
        ));
    }
    Line::from(spans)
}

/// Map a row tint to its color slot (the glyph table's partner — the
/// accessibility triple-encoding: meaning never rides shape alone).
pub(super) fn tint_color(tint: RowTint) -> Color {
    match tint {
        RowTint::Running => Color::Cyan,
        RowTint::Done => Color::Green,
        RowTint::Error => Color::Red,
        RowTint::Warn => Color::Yellow,
    }
}

/// The framed panel: three columns `[sel][glyph][name]  [files]  [activity]`
/// (design mock; the time column is gone — done rows carry `done 2m13s` in
/// their activity cell), separated by whitespace only (task_layout::
/// column_gap), squeezed right→left as the panel narrows
/// (task_layout::column_widths). S1 draws dim borders and no cursor;
/// S2 draws the `>` cursor on the selected row with normal borders. Rows are
/// the design's 6-row scroll window (`window_offset`), spawn-stable.
pub(super) fn render_frame_face(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme();
    let views = app.task_row_views();
    let total = views.len();
    let in_s2 = matches!(app.task_panel.focus, FocusTarget::TaskList(_));

    // Title: the live row count plus the unacked-failure badge (cleared on
    // S2 entry — D10); the close hint rides the top border's right end (one
    // of only two `^T` homes). The wording lives with the string table.
    let title = task_layout::panel_title(total, app.task_panel.unacked_failures.len());
    // #3 任务面板框只留上边框: the frame keeps its top rule only — the rows
    // read as a section under that rule, not as a box. The title and the
    // close hint ride that one rule, so the row budget shrinks by one. Both
    // are flanked by a rule stub and inset from the panel's endpoints
    // (横条夹字，不顶两端) — the labels read as stations along the rule, not
    // as caps glued to its ends.
    let mut block = Block::default()
        .borders(Borders::TOP)
        .border_type(BorderType::Plain)
        .title(format!("─ {title} "))
        .title(
            Line::from(Span::styled(
                format!(" {} ─", task_layout::FRAME_CLOSE_HINT),
                Style::default().fg(theme.hint),
            ))
            .right_aligned(),
        );
    if !in_s2 {
        // S1: visible but nothing selected — the frame dims (theme.hint).
        block = block.border_style(Style::default().fg(theme.hint));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    if total == 0 {
        // Safety net: the toggle never opens an empty frame (框跟着事走 — the frame follows the work),
        // but a frame whose rows all left under it says so here.
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No sub-tasks yet",
                Style::default().fg(theme.hint),
            ))),
            inner,
        );
        app.task_panel.view_rows = inner.height as usize;
        return;
    }

    // The 6-row scroll window; record what fit so page steps match the frame.
    let view_rows = inner.height as usize;
    app.task_panel.view_rows = view_rows;
    let off = app
        .task_panel
        .window_offset
        .min(total.saturating_sub(view_rows.max(1)));
    let end = (off + view_rows).min(total);

    // Columns in DISPLAY COLUMNS: name 12–28 (`Elide::Head` — the tail of a
    // name is its least identifying part), files 12, activity flex. The
    // sel/glyph slots are fixed chrome; the columns stand apart on
    // whitespace (column_gap), which widens with the panel so the row does
    // not read as one left-packed clump.
    let inner_w = inner.width as usize;
    let name_wanted = views
        .iter()
        .map(|v| UnicodeWidthStr::width(v.name.as_str()))
        .max()
        .unwrap_or(0);
    let gap = task_layout::column_gap(inner_w, name_wanted);
    // marker+" ", glyph+" ", two inter-column gaps.
    let cols_budget = inner_w.saturating_sub(4 + 2 * gap);
    let (name_w, files_w, act_w) = task_layout::column_widths(cols_budget, name_wanted);

    // The frame's window over the display set; `view_rows` is recorded so
    // page steps match what fit. Row rendering is shared with the live face.
    let lines: Vec<Line> = views
        .iter()
        .take(end)
        .skip(off)
        .map(|v| task_row_line(v, name_w, Some(files_w), gap, act_w, &theme))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// Inspection view for a focused background task row: the task's live output
/// tail (last ≤2KB of stdout+stderr, refreshed per tick while running) in an
/// auto-scrolling window — `tail -f` without leaving the TUI.
pub(super) fn render_bg_tail(f: &mut Frame, app: &App, area: Rect, task_id: &str) {
    let theme = app.theme();
    let dim = Style::default().fg(theme.faint);
    let Some(entry) = app.background_tasks.get(task_id) else {
        return;
    };
    let (glyph, tint) = task_layout::bg_glyph(&entry.status);
    let color = tint_color(tint);
    let elapsed = match entry.finished_at {
        Some(at) => at.duration_since(entry.started_at),
        None => entry.started_at.elapsed(),
    };
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let lines: Vec<String> = entry
        .output_tail
        .lines()
        .flat_map(|l| wrap(l, inner_w))
        .collect();
    let title = Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color)),
        Span::styled(
            elide(
                &entry.command,
                inner_w.saturating_sub(24).max(8),
                Elide::Head,
            ),
            dim,
        ),
        Span::styled(
            format!(" - {}", task_layout::bg_status_word(&entry.status)),
            Style::default().fg(color),
        ),
        Span::styled(format!(" - {}", format_time(elapsed)), dim),
    ]);
    f.render_widget(
        phi_tui::tail_panel::TailPanel {
            title,
            lines: &lines,
            line_style: Style::default().fg(theme.muted),
            border_style: dim,
        },
        area,
    );
}

/// Rows a content line takes at `inner_w` columns once wrapped (display width).
/// Word wrap can only rarely add a row on top of this — and when it does, the
/// body clips (fail-safe) while the footer stays put.
pub(super) fn wrapped_rows(s: &str, inner_w: u16) -> u16 {
    if s.is_empty() {
        return 1;
    }
    UnicodeWidthStr::width(s)
        .max(1)
        .div_ceil(inner_w.max(1) as usize) as u16
}
