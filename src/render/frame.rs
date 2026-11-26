//! Outer frame: composer, status strip, process line, thinking panel.

use super::*;

pub fn draw(f: &mut Frame, app: &mut App) {
    // Output area has no border — use the full terminal width for wrapping.
    let term_w = f.area().width;
    let content_width = term_w as usize;
    app.set_wrap_width(content_width);

    // Composer height: visual rows (accounting for soft-wrap) + 2 for the
    // top/bottom rules. Claude Code style chrome: TOP+BOTTOM only, so no
    // columns are lost to side borders (inner width = terminal width).
    let composer_inner_w = term_w;
    let composer_vis = app
        .composer
        .visual_height(composer_inner_w)
        .min(MAX_COMPOSER_ROWS);
    let composer_height = composer_vis as u16 + 2;
    let frame_open = app.task_panel.frame_open;

    tracing::debug!(
        term_w,
        content_width,
        composer_inner_w,
        composer_vis,
        composer_height,
        frame_open,
        "draw layout"
    );

    // Queued-message strip: only while something is actually waiting (its
    // rows are the preview that replaces the echo at delivery time).
    let queued_rows = app.queued_strip_lines();
    // Pasted screenshot chips (unsent attachments), one row per image, always
    // directly above the composer they will leave from.
    let chip_rows = app.pending_image_labels();

    let mut constraints = vec![
        Constraint::Min(3),    // output (transcript)
        Constraint::Length(1), // gap: transcript content never crowds the process row
        Constraint::Length(1), // process status line (status + tip, Claude Code style)
    ];
    // Task face (Rev 2026-11-04): one row region, two modes — the framed
    // panel when open, else the frameless live face (the count strip and its
    // standing hint are retired). Both cap at 6 rows and shrink on short
    // terminals; below one row the region is suppressed — the status line
    // always survives (矮终端).
    let task_region_h = {
        let fixed = 1 + 1 + queued_rows.len() as u16 + chip_rows.len() as u16 + composer_height + 1;
        let avail = f.area().height.saturating_sub(fixed + 3); // Min(3) output floor
        if frame_open {
            task_layout::frame_height(app.task_row_count(), avail)
        } else if app.should_show_task_panel() {
            let (shown, hidden_more, hidden_done) = live_face_split(&app.task_row_views());
            task_layout::live_face_height(shown.len(), hidden_more + hidden_done > 0, avail)
        } else {
            0
        }
    };
    if task_region_h > 0 {
        constraints.push(Constraint::Length(task_region_h)); // task face
    }
    if !queued_rows.is_empty() {
        constraints.push(Constraint::Length(queued_rows.len() as u16)); // queued strip
    }
    if !chip_rows.is_empty() {
        constraints.push(Constraint::Length(chip_rows.len() as u16)); // image chips
    }
    // The composer's own top/bottom rules are the separators around it —
    // no blank rows between the process band, the rules, and the mode line.
    // (A cell grid has no half-row; the rules carry the visual separation
    // and keep the status and mode lines one keystroke away from the box.)
    constraints.push(Constraint::Length(composer_height)); // composer
    constraints.push(Constraint::Length(1)); // status bar (mode line)

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(f.area());

    // Record the output pane rect so mouse events can hit-test transcript rows.
    let out = chunks[0];
    app.output_area = Some((out.x, out.y, out.width, out.height));

    let mut idx = 0;
    render_output(f, app, chunks[idx]);
    idx += 2; // skip the gap above the process row
    render_process_line(f, app, chunks[idx]);
    idx += 1;
    if task_region_h > 0 {
        render_task_face(f, app, chunks[idx]);
        idx += 1;
    }
    if !queued_rows.is_empty() {
        render_queued_strip(f, app, chunks[idx], &queued_rows);
        idx += 1;
    }
    if !chip_rows.is_empty() {
        render_image_chips(f, app, chunks[idx], &chip_rows);
        idx += 1;
    }
    let composer_area = chunks[idx];
    render_composer(f, app, composer_area);
    render_status(f, app, chunks[idx + 1]);

    if app.has_pending_approval() {
        render_approval_popup(f, app);
    } else if app.has_pending_ask() {
        // One modal at a time: an approval in front hides the question behind
        // it, and the approval's key rules own the keyboard until it clears.
        render_ask_popup(f, app);
    } else if app.has_pending_pick() {
        render_pick_popup(f, app);
    }
    if app.context_menu().is_some() {
        render_context_menu(f, app);
    }
    if app.mention().is_some() {
        render_mention_popup(f, app, composer_area);
    }
    if app.slash().is_some() {
        render_slash_popup(f, app, composer_area);
    }
}

/// Queued-message strip (above the composer): the pending steering messages
/// and held commands, one row each. Dimmed chrome — the real echo happens in
/// the transcript exactly once, at delivery (or at the turn-end flush). The
/// rows themselves carry the `queued ->` / `queued after turn ->` prefixes
/// (built by `App::queued_strip_lines`, which also bounds the row count).
pub(super) fn render_queued_strip(f: &mut Frame, app: &App, area: Rect, rows: &[String]) {
    let theme = app.theme();
    let lines: Vec<Line> = rows
        .iter()
        .map(|row| Line::from(Span::styled(row.clone(), Style::default().fg(theme.hint))))
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// Pasted-screenshot chips (one row per pending image, above the composer).
/// The chip is the attachment's face until the send consumes it — same
/// dimmed chrome as the queued strip.
pub(super) fn render_image_chips(f: &mut Frame, app: &App, area: Rect, rows: &[String]) {
    let theme = app.theme();
    let lines: Vec<Line> = rows
        .iter()
        .map(|row| Line::from(Span::styled(row.clone(), Style::default().fg(theme.hint))))
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

pub(super) fn render_composer(f: &mut Frame, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize; // top+bottom rules
    let inner_width = area.width as usize; // no side borders (Claude Code style)
    let prefix_w = 2usize; // "> " or "  "

    tracing::debug!(
        comp_area_w = area.width,
        inner_width,
        inner_height,
        "render_composer"
    );
    let lines = app.composer.lines();
    let cursor = app.composer.cursor();

    // Pre-wrap each logical line to `content_w` columns so each `Line` maps
    // to exactly one ratatui row. This avoids double-wrapping (our slice +
    // Paragraph::wrap) which caused misaligned display.
    let mut items: Vec<Line> = Vec::new();
    // Caret position in (wrapped-row, column) space — and, once the box is
    // laid out, on screen. The caret is the REAL terminal cursor (see below),
    // not a REVERSED stand-in.
    let mut caret_item: Option<usize> = None;
    let mut caret_col: usize = 0;
    let mut item_base = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let prefix = if idx == 0 { "> " } else { "  " };
        let full = format!("{prefix}{line}");
        let wrapped = wrap(&full, inner_width);

        // Find which visual row the cursor falls on and its column in that row.
        let cursor_pos = if idx == cursor.0 {
            let cursor_byte = prefix_w + cursor.1;
            let mut col = 0usize;
            let mut cur_row = 0usize;
            let mut cur_col_in_row = 0usize;
            let mut byte_offset = 0usize;
            let mut found = None;
            for ch in full.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if col > 0 && col + cw > inner_width {
                    // Wrap: new row starts with this char.
                    col = 0;
                    cur_row += 1;
                    cur_col_in_row = 0;
                }
                if byte_offset >= cursor_byte {
                    // Cursor is at this column in this row.
                    found = Some((cur_row, cur_col_in_row));
                    break;
                }
                col += cw;
                cur_col_in_row += cw;
                byte_offset += ch.len_utf8();
            }
            // Cursor at end of text: place on last row at end.
            if found.is_none() {
                found = Some((cur_row, cur_col_in_row));
            }
            found
        } else {
            None
        };

        for (j, row) in wrapped.iter().enumerate() {
            if let Some((vis_row, vis_col)) = cursor_pos
                && j == vis_row
            {
                caret_item = Some(item_base + j);
                caret_col = vis_col;
            }
            items.push(Line::from(row.clone()));
        }
        item_base += wrapped.len();
    }

    // Scroll to show the bottom of the buffer (cursor is usually at the end).
    let total = items.len();
    let scroll = total.saturating_sub(inner_height) as u16;

    // Claude Code style: full-width horizontal rules, open sides. `faint`
    // keeps the rules at notice level on dark terminals and steps darker on
    // light ones (theme slot, follows --color-scheme).
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(app.theme().faint));
    f.render_widget(Paragraph::new(items).block(block).scroll((scroll, 0)), area);

    // Park the REAL terminal cursor on the input caret. A REVERSED stand-in
    // draws the caret but leaves the hardware cursor wherever the last diff
    // cell landed — after a scroll redraw that is the status bar — and macOS
    // Terminal.app draws the IME composition (marked text) at that stale
    // position, so typed CJK shows up in the wrong place until Ctrl+L. The
    // terminal's own cursor is what anchors an IME; show it instead of faking
    // it (a block cursor over a REVERSED cell double-inverts and vanishes).
    if let Some(item) = caret_item
        && inner_height > 0
    {
        let visible_row = (item as u32).saturating_sub(scroll as u32);
        if visible_row < inner_height as u32 {
            let x = area.x + (caret_col as u16).min(area.width.saturating_sub(1));
            let y = area.y + 1 + visible_row as u16;
            f.set_cursor_position((x, y));
        }
    }
}

/// The process-status line (Claude Code style) that sits between the
/// transcript and the composer: live phase (thinking/streaming/tool/waiting)
/// or, at Idle, the ambient notices. A running spin is still shown live so an
/// event-less LLM call isn't mistaken for a frozen screen. The current tip
/// rides the same row, right behind the status.
pub(super) fn render_process_line(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let width = area.width as usize;
    // The squeeze ladder (task_layout::fit_status) decides how much text
    // survives at this width and whether the Tip still has room behind it.
    let fit = app.status_fit(width);
    // The busy band rides the brand's bright warm (`warm_bright`, #4) — one
    // step above the `auto mode on` badge at the bottom band, which keeps
    // `warm` (dogfood 20261104: hardcoded Cyan/Yellow fought the badge).
    // Waiting vs executing lives in the lead text; the one blocking ask
    // (approval) earns the palette's red.
    let style = match app.status {
        AgentStatus::Idle => Style::default().fg(theme.hint),
        AgentStatus::Running {
            phase: Phase::AwaitingApproval,
        } => Style::default().fg(theme.error),
        AgentStatus::Waiting { .. } | AgentStatus::Running { .. } => {
            Style::default().fg(theme.warm_bright)
        }
    };
    // #4 配色分层: the live phase word is the one fact on the row, so it keeps
    // the row's own color; the parenthetical hint beside it drops to the gray
    // hint slot. The split point is the ladder's (fit.tail_start) — the tail
    // is a parenthetical clause, never part of the phase word. The lead wears
    // `style` verbatim: the approval row's red head must survive the split.
    let mut spans = if let Some(tail_start) = fit.tail_start.filter(|s| *s > 0) {
        let (lead, tail) = fit.text.split_at(tail_start);
        vec![
            Span::styled(lead.to_string(), style),
            Span::styled(tail.to_string(), Style::default().fg(theme.hint)),
        ]
    } else {
        vec![Span::styled(fit.text.clone(), style)]
    };
    // The tip follows the status after a tab-wide gap so it never crowds or
    // replaces it. What remains after the fitted status is the tip's budget;
    // `Elide::Head` keeps its `Tip:`/`Note:` label.
    if fit.tip_allowed
        && app.tip_visible()
        && let Some(tip) = tip_line_text(app)
    {
        // The status may be split into several spans (lead + tail), so the
        // tip's budget counts all of them — not just the first.
        let status_w: usize = spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        let budget = width.saturating_sub(status_w + task_layout::TIP_GAP);
        let tip = elide(&tip, budget, Elide::Head);
        spans.push(Span::raw(" ".repeat(task_layout::TIP_GAP)));
        spans.push(Span::styled(tip, Style::default().fg(theme.hint)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The tip text for the process row: `Tip: ...`, or `Note: ...` for items
/// labelled as promos/announcements (never dressed up as a Tip).
pub(super) fn tip_line_text(app: &App) -> Option<String> {
    let tip = app.current_tip()?;
    Some(format!("{}: {}", tip.kind.label(), tip.text))
}

/// Bottom status bar (Claude Code style): left, the live approval mode badge
/// with its switch hint plus the design-canvas face (1C row 13); right, the
/// highest-priority status-driven notice — the persistent upgrade hint, else
/// the transcript scroll position. The right slot stays empty when there is
/// nothing to report; rotating tips ride the process row, never here.
pub(super) fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let mode = app.approval_switch.get();
    // auto and ask are equally calm positions of one switch, so they share the
    // brand warm badge color; only deny is a warning (red).
    let badge_style = match mode {
        crate::approval::RuntimeApprovalMode::Auto | crate::approval::RuntimeApprovalMode::Ask => {
            Style::default().fg(theme.warm)
        }
        crate::approval::RuntimeApprovalMode::Deny => Style::default().fg(Color::Red),
    };
    let mut spans = vec![
        // T14: the badge words are product copy (ApprovalCopy::mode_badge);
        // the severity color above stays shell mechanism.
        Span::styled((app.copy.approval.mode_badge)(&mode), badge_style),
        Span::raw(" "),
        Span::styled(
            app.copy.mode_cycle_hint.as_str(),
            Style::default().fg(theme.hint),
        ),
    ];
    // Canvas face (1C): the spec-locked state words, green when the canvas is
    // simply there and red-bold on the crash face (row 16).
    if let Some(status) = &app.panel_status {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            status.text.clone(),
            if status.alert {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.good)
            },
        ));
    }
    if let Some((text, style)) = status_bar_right(app) {
        let width = area.width as usize;
        let left_w: usize = spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        // The mode block is short and structural — it keeps its columns and
        // the right notice elides to what remains (head kept, like tips).
        if width > left_w + 2 {
            let text = elide(&text, width - left_w - 2, Elide::Head);
            let text_w = UnicodeWidthStr::width(text.as_str());
            spans.push(Span::raw(" ".repeat(width - left_w - text_w)));
            spans.push(Span::styled(text, style));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// The mode badge's switch hint lives in `CopyTable::mode_cycle_hint` — product
// wording now (T5c), rendered here unchanged.

/// The right slot of the bottom status bar, in priority order: the upgrade
/// hint (persistent), else the scroll position while the transcript sits off
/// the bottom (with the key that re-follows it). `None` when there is nothing
/// to report. Both ride the medium green `good` slot.
pub(super) fn status_bar_right(app: &App) -> Option<(String, Style)> {
    let green = Style::default().fg(app.theme().good);
    if let Some(hint) = &app.upgrade_hint {
        return Some((hint.clone(), green));
    }
    let v = &app.viewport;
    if v.follow_bottom || v.rendered_total == 0 || v.viewport_height == 0 {
        return None;
    }
    let total = v.rendered_total;
    let range = v.window_range(total, v.viewport_height);
    Some(((app.copy.scroll_hint)(range.start + 1, total), green))
}

/// The live thinking panel: bordered, internally scrolling (last lines of the
/// pending thought), title carries spinner / agent / elapsed / token estimate.
pub(super) fn render_thinking_panel(
    f: &mut Frame,
    app: &App,
    area: Rect,
    lines: &[String],
    agent: Option<&str>,
    chars: usize,
) {
    let dim = Style::default().fg(app.theme().faint);
    let spinner = app.spinner_char();
    // Per-stream timer (`""` = root, matching the agent_id routing key).
    let key = agent.unwrap_or("");
    let elapsed = app
        .thinking_since
        .get(key)
        .map_or(0, |t| t.elapsed().as_secs() + 1);
    let mut title = vec![Span::styled(format!("{spinner} thinking"), dim)];
    if let Some(id) = agent {
        title.push(Span::styled(format!(" - {id}"), dim));
    }
    title.push(Span::styled(
        format!(
            " - {} - ~{} tokens (est)",
            App::fmt_elapsed(elapsed),
            fmt_k(chars as u64 / 3)
        ),
        dim,
    ));
    f.render_widget(
        phi_tui::tail_panel::TailPanel {
            title: Line::from(title),
            lines,
            line_style: style_for(LineKind::Thought, app.theme()),
            border_style: dim,
        },
        area,
    );
}
