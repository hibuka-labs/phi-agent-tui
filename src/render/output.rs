//! Transcript and streaming-tail rendering.

use super::*;

pub(super) fn render_output(f: &mut Frame, app: &mut App, area: Rect) {
    let height = area.height as usize;

    // Choose which transcript to display based on focus: a sub-agent row
    // shows its transcript, a background row shows that task's live output
    // tail (the inspection view), anything else the root transcript.
    // Rows land by id (Rev 2026-11-04) — indices shift when rows hide.
    let focused_row: Option<TaskRow> = match &app.task_panel.focus {
        FocusTarget::TaskList(id) => app.task_rows().into_iter().find(|r| r.id() == id.as_str()),
        FocusTarget::Input => None,
    };
    if let Some(TaskRow::Background(task_id)) = &focused_row {
        // Auto-scrolling tail window, not a transcript: no selection, no
        // viewport bookkeeping — `app.visual_map` is deliberately left at the
        // last root frame so the scroll anchor survives the visit.
        render_bg_tail(f, app, area, task_id);
        return;
    }
    let focused_agent: Option<String> = match focused_row {
        Some(TaskRow::SubAgent(id)) => Some(id),
        _ => None,
    };
    let (transcript, is_sub_agent) = match &focused_agent {
        Some(id) => match app.sub_agent_transcripts.get(id) {
            Some(sub_transcript) => (sub_transcript.as_slice(), true),
            None => (app.transcript.output.as_slice(), false),
        },
        None => (app.transcript.output.as_slice(), false),
    };

    // Streaming tail: only pending PROSE still renders inline (markdown, as
    // before). A pending THOUGHT lives in the thinking panel inline at the
    // flow tail (8 placeholder rows — see the panel block below) — the old
    // full-tail inline render was the wall of scrolling dim text that panel
    // replaces. A focused child shows ITS live tail — the whole point of
    // following a sub-agent is watching it work (e.g. the long silent
    // report-writing stretch with no tool-call boundary).
    let tail_raw: Option<(String, LineKind)> = if is_sub_agent {
        let id = focused_agent
            .as_deref()
            .expect("is_sub_agent implies focus");
        app.child_stream_tail_raw(id)
    } else {
        app.streaming_tail_raw()
            .map(|(raw, kind)| (raw.to_string(), kind))
    };

    let committed = transcript.len();

    // Pre-render committed lines and streaming tail into a flat vec.
    let mut lines: Vec<Line> = Vec::with_capacity(committed + 32);
    // Row bookkeeping (visual row -> output index + plain text for copy) in
    // one aligned map; tail/panel rows are unselectable. See `phi_tui::visual`.
    let mut visual_map = phi_tui::visual::VisualMap::with_capacity(committed + 32);

    // --- committed output ---
    // Stylesheet for this frame (resolved once at startup): every
    // gray/truecolor site below reads its value from `theme` slots.
    let theme = app.theme();
    // Which lines open a block. Decided up front so the detail branches below
    // don't each have to carry rhythm state through their `continue`s.
    let opens: Vec<bool> = {
        let mut v = Vec::with_capacity(committed);
        let mut prev: Option<&phi_tui::lines::OutputLine<ShellToken>> = None;
        for line in transcript.iter() {
            v.push(prev.is_none_or(|p| opens_block(p, line)));
            prev = Some(line);
        }
        v
    };
    let mut vis_idx: usize = 0; // running visual line counter
    // User-message fold (#8): long input (pasted logs, files) collapses to
    // head rows + a "... +N lines hidden" marker + tail rows. Per message (a
    // run of User rows starting at one that carries `original`), render-time
    // only — the transcript keeps every row, and Ctrl+O (`show_thoughts`, the
    // switch that expands thinking) unfolds everything. `fold_marker` is
    // keyed by the first hidden row: that is where the marker stands and
    // what it maps to.
    let mut fold_marker: Vec<Option<(usize, usize)>> = vec![None; committed];
    if !app.show_thoughts {
        let mut i = 0;
        while i < committed {
            if transcript[i].kind != LineKind::User || transcript[i].original.is_none() {
                i += 1;
                continue;
            }
            let start = i;
            // The message's rows run until the next non-User row or the next
            // message head (a User row carrying `original`).
            let mut end = i + 1;
            while end < committed
                && transcript[end].kind == LineKind::User
                && transcript[end].original.is_none()
            {
                end += 1;
            }
            if end - start > USER_FOLD_THRESHOLD {
                let head_end = start + USER_FOLD_HEAD;
                let tail_start = end - USER_FOLD_TAIL;
                fold_marker[head_end] = Some((tail_start, tail_start - head_end));
            }
            i = end;
        }
    }
    let mut fold_skip_to: Option<usize> = None;
    for i in 0..committed {
        // The fold marker stands in for the hidden middle of a long message.
        if let Some((tail_start, hidden)) = fold_marker[i] {
            // ASCII `...`: `…` is CJK-width-unsafe (render_tests.rs).
            let hint = format!("... +{hidden} lines hidden (Ctrl+O expand)");
            let mut styled =
                Line::from(Span::styled(hint.clone(), Style::default().fg(theme.faint)));
            apply_selection(&mut styled, app, vis_idx);
            visual_map.push_mapped(i, hint);
            lines.push(styled);
            vis_idx += 1;
            fold_skip_to = Some(tail_start);
            continue;
        }
        if let Some(skip) = fold_skip_to {
            if i < skip {
                continue;
            }
            fold_skip_to = None;
        }
        // Block spacer. Render-time only: it is a row on the screen and not a
        // row in `transcript.output`, so history, selection indices,
        // `follow_bottom`, export and resume digests never see it (the same
        // rule the thinking fold follows).
        if i > 0 && opens[i] {
            lines.push(Line::from(Span::raw("")));
            visual_map.push_unselectable(String::new());
            vis_idx += 1;
        }
        let line = &transcript[i];
        let kind = line.kind;
        let spans = line.spans.as_deref().unwrap_or(&[]);

        // Diff detail: render the invocation line, then expand the diff block.
        if let Some(ref detail) = line.detail {
            match detail {
                phi_tui::lines::LineDetail::Diff { path, hunks } => {
                    // Invocation line: same composed tool row as everywhere
                    // else (state glyph + name + grey args).
                    let mut styled = tool_invocation_line(
                        &line.text,
                        line.tool_state,
                        app.spinner_char(),
                        theme,
                    );
                    apply_selection(&mut styled, app, vis_idx);
                    visual_map.push_mapped(i, line_plain_text(&styled));
                    lines.push(styled);
                    vis_idx += 1;

                    // Diff header: "┌─ path"
                    let header_text = format!("┌─ {path}");
                    let mut header_line = Line::from(Span::styled(
                        header_text.clone(),
                        Style::default().fg(theme.faint),
                    ));
                    apply_selection(&mut header_line, app, vis_idx);
                    visual_map.push_mapped(i, header_text);
                    lines.push(header_line);
                    vis_idx += 1;

                    // Hunk lines
                    let wrap_w = app.transcript.wrap_width().saturating_sub(8).max(20);
                    // Compute max line number width for alignment
                    let max_line = hunks
                        .iter()
                        .flat_map(|h| h.lines.iter())
                        .flat_map(|l| [l.old_line, l.new_line])
                        .flatten()
                        .max()
                        .unwrap_or(1);
                    let num_w = format!("{max_line}").len();

                    for hunk in hunks {
                        // Hunk header: "@@ -a,b +c,d @@"
                        let hunk_text = format!("│ {:>num_w$} {}", "", hunk.header);
                        let mut hunk_line = Line::from(Span::styled(
                            hunk_text.clone(),
                            Style::default().fg(Color::Cyan),
                        ));
                        apply_selection(&mut hunk_line, app, vis_idx);
                        visual_map.push_mapped(i, hunk_text);
                        lines.push(hunk_line);
                        vis_idx += 1;

                        for dl in &hunk.lines {
                            let (sign, color) = match dl.kind {
                                phi_tui::lines::DiffLineKind::Add => ("+", Color::Green),
                                phi_tui::lines::DiffLineKind::Del => ("-", Color::Red),
                                phi_tui::lines::DiffLineKind::Context => (" ", theme.faint),
                            };
                            // Line number: prefer old_line for context/del, new_line for add
                            let line_num = match dl.kind {
                                phi_tui::lines::DiffLineKind::Add => dl.new_line,
                                phi_tui::lines::DiffLineKind::Del => dl.old_line,
                                phi_tui::lines::DiffLineKind::Context => dl.old_line,
                            };
                            let num_str = match line_num {
                                Some(n) => format!("{n:>num_w$}"),
                                None => " ".repeat(num_w),
                            };
                            // Wrap long lines, indent continuations
                            let full = format!("│ {num_str} {sign} {}", dl.text);
                            let cont_prefix = format!("│ {} {sign} ", " ".repeat(num_w));
                            let wrapped = wrap(&full, wrap_w + 8);
                            for (wi, wline) in wrapped.iter().enumerate() {
                                let display = if wi == 0 {
                                    wline.clone()
                                } else {
                                    format!("{cont_prefix}{wline}")
                                };
                                let mut styled = Line::from(Span::styled(
                                    display.clone(),
                                    Style::default().fg(color),
                                ));
                                apply_selection(&mut styled, app, vis_idx);
                                visual_map.push_mapped(i, display);
                                lines.push(styled);
                                vis_idx += 1;
                            }
                        }
                    }
                    continue; // skip the normal rendering path below
                }
                phi_tui::lines::LineDetail::Folded {
                    raw,
                    line_count,
                    char_count,
                    duration_secs,
                    meta_head,
                } => {
                    let width = app.transcript.wrap_width();
                    // ── thinking ──────────────────────────────────────────
                    // Folded by default (one summary line); Ctrl+O expands to
                    // the full text re-wrapped at the current width. Folding
                    // is a render-time decision — the line always carries the
                    // full text in `detail.raw`.
                    if kind == LineKind::Thought {
                        if !app.show_thoughts {
                            // No leading `>`: that is the user line's marker
                            // (transcript.rs `push_user`) and a summary that
                            // starts with `>` reads as something you typed.
                            // The expand hint rides every summary row: the
                            // fold is otherwise indistinguishable from a
                            // dropped segment (discoverability of Ctrl+O).
                            let mut summary = format!(
                                "thinking - {line_count} line{} - ~{} tokens (est)",
                                if *line_count == 1 { "" } else { "s" },
                                fmt_k(*char_count as u64 / 3)
                            );
                            if let Some(secs) = duration_secs {
                                summary.push_str(&format!(" - {}", App::fmt_elapsed(*secs)));
                            }
                            summary.push_str(&app.copy.thought_expand_hint);
                            let mut styled = Line::from(Span::styled(
                                summary.clone(),
                                style_for(LineKind::Thought, theme),
                            ));
                            apply_selection(&mut styled, app, vis_idx);
                            visual_map.push_mapped(i, summary);
                            lines.push(styled);
                            vis_idx += 1;
                            continue;
                        }
                        for (wline, seam) in wrap_rows(raw, width) {
                            let mut styled = Line::from(Span::styled(
                                wline.clone(),
                                style_for(LineKind::Thought, theme),
                            ));
                            apply_selection(&mut styled, app, vis_idx);
                            visual_map.push_seamed(i, wline, seam);
                            lines.push(styled);
                            vis_idx += 1;
                        }
                        continue;
                    }

                    // ── tool block: the preview ladder (design §4) ────────
                    // The meta row is already in `text`. Everything under it is
                    // the payload, shown 0 / 2 / all rows by `result_tier`.
                    // Only display changes: `raw` always holds the tool's
                    // answer whole, so the transcript, selection and copy are
                    // unaffected by which tier is on.
                    let meta = if kind == LineKind::Tool {
                        tool_invocation_line(&line.text, line.tool_state, app.spinner_char(), theme)
                    } else {
                        result_meta_line(&line.text, kind, theme)
                    };
                    let mut styled = meta;
                    apply_selection(&mut styled, app, vis_idx);
                    visual_map.push_mapped(i, line_plain_text(&styled));
                    lines.push(styled);
                    vis_idx += 1;

                    // Which payload rows the tier owes: the meta row's
                    // relationship to `raw`'s head is recorded at commit time
                    // (`MetaHead`), so nothing is re-derived from the width
                    // here. Wrapping/indenting is ours; row selection is
                    // `phi_tui::layout`.
                    let preview = match app.result_tier {
                        ResultTier::Compact => Preview::Rows(0),
                        ResultTier::Default => Preview::Rows(2),
                        ResultTier::Expanded => Preview::All,
                    };
                    let fold = folded_body(raw, *meta_head, preview);
                    let inner = width.saturating_sub(FOLD_INDENT.len()).max(20);
                    let mut payload: Vec<String> = Vec::new();
                    // An abbreviated head comes back in full at Expanded —
                    // otherwise "expand" would still be hiding text. It is
                    // drawn *first*, so the expanded payload keeps the
                    // answer's own order (design: order may never change).
                    if let Some(head) = fold.restore_head {
                        payload.extend(wrap(head, inner));
                    }
                    for row in &fold.rows {
                        payload.extend(wrap(row, inner));
                    }
                    for wline in payload {
                        let display = format!("{FOLD_INDENT}{wline}");
                        let mut styled =
                            Line::from(Span::styled(display.clone(), style_for(kind, theme)));
                        apply_selection(&mut styled, app, vis_idx);
                        visual_map.push_mapped(i, display);
                        lines.push(styled);
                        vis_idx += 1;
                    }
                    if fold.hidden > 0 {
                        // ASCII `...`: `…` is CJK-width-unsafe (render_tests.rs).
                        let hint = format!(
                            "{FOLD_INDENT}... +{} line{}",
                            fold.hidden,
                            if fold.hidden == 1 { "" } else { "s" }
                        );
                        let mut styled = Line::from(Span::styled(
                            hint.clone(),
                            Style::default().fg(theme.faint),
                        ));
                        apply_selection(&mut styled, app, vis_idx);
                        visual_map.push_mapped(i, hint);
                        lines.push(styled);
                        vis_idx += 1;
                    }
                    continue;
                }
            }
        }

        if kind == LineKind::Normal && spans.is_empty() {
            // Markdown emits one styled Line per paragraph and the transcript
            // `Paragraph` does not soft-wrap — an over-wide line is clipped at
            // the pane edge, so the tail of a long answer disappears from the
            // screen. Wrap each markdown row into visual rows (span-level,
            // style-preserving) and map every row back to source line `i`.
            let width = app.transcript.wrap_width();
            // The width budget is what keeps a table a table: without it a
            // long cell makes the box wider than the pane and the
            // soft-wrap below shreds the borders.
            let md_lines = render_markdown_with_width(&line.text, Some(width));
            for md_line in md_lines {
                for (mut wrapped, seam) in wrap_line_rows(md_line, width) {
                    apply_selection(&mut wrapped, app, vis_idx);
                    visual_map.push_seamed(i, line_plain_text(&wrapped), seam);
                    lines.push(wrapped);
                    vis_idx += 1;
                }
            }
        } else {
            // A tool invocation is composed (state glyph + name + grey args),
            // a result row splits the connector from the answer — only these
            // two need anything fancier than "paint the whole line".
            let mut styled = if kind == LineKind::Tool {
                tool_invocation_line(&line.text, line.tool_state, app.spinner_char(), theme)
            } else if kind == LineKind::ToolResult {
                result_meta_line(&line.text, kind, theme)
            } else if kind == LineKind::System && spans.is_empty() {
                // Command answers (usage hints, /tools, /connect, install
                // results) are plain System text but not uniform: a status
                // glyph takes its hue, a column-aligned label reads strong.
                // Banner and verdict rows carry spans and keep `span_line`.
                system_info_line(&line.text, theme)
            } else {
                span_line(&line.text, spans, style_for(kind, theme), theme)
            };
            // Exempt `push_styled_line` rows (spans + no `original`) from the
            // wrap below. They are fixed-width chrome — banner wordmark art and
            // the full-width `─` rule — and phi-tui's `push_styled_line`
            // contract is "**no** soft-wrap (the wordmark must stay whole;
            // ratatui clips on narrow terminals), no re-wrap on width change
            // (`original: None`)". Wrapping breaks that in two ways: the rule
            // has no whitespace to break at, so it hard-splits into dash
            // fragments, and the 60-col wordmark splits at its letter gaps.
            // Emit as ONE visual row; ratatui clips at the pane edge.
            if line.spans.is_some() && line.original.is_none() {
                apply_selection(&mut styled, app, vis_idx);
                visual_map.push_mapped(i, line_plain_text(&styled));
                lines.push(styled);
                vis_idx += 1;
            } else {
                // Same wrap contract as the markdown path: an over-wide tool or
                // attributed line is clipped by the pane otherwise (lines already
                // shortened with `elide`/`one_line` fit and take the fast path).
                let width = app.transcript.wrap_width();
                for (mut wrapped, seam) in wrap_line_rows(styled, width) {
                    apply_selection(&mut wrapped, app, vis_idx);
                    visual_map.push_seamed(i, line_plain_text(&wrapped), seam);
                    lines.push(wrapped);
                    vis_idx += 1;
                }
            }
        }
    }

    // --- streaming tail (prose only) ---
    if let Some((raw, LineKind::Normal)) = &tail_raw {
        // Same wrap contract as the committed markdown path above: the live
        // tail is uncommitted prose, and clipping it hides the end of an
        // in-flight answer until it commits.
        let width = app.transcript.wrap_width();
        let md_lines = render_markdown_with_width(raw, Some(width));
        for md_line in md_lines {
            for wrapped in wrap_line(md_line, width) {
                visual_map.push_unselectable(line_plain_text(&wrapped));
                lines.push(wrapped);
            }
        }
    }

    // In-flight thinking: two presentations behind Ctrl+O (`show_thoughts`),
    // one toggle flipping both live thinking AND history thoughts together.
    // Loose (on): stream every wrapped row of the thought straight into the
    // flow — print as it thinks, no box (the pre-panel behavior) — history
    // thoughts expand alongside. Children carry `[{agent}] ` on the first
    // row (loose has no panel title to name the author); the prefix is the
    // same one `flush_thought` bakes into `detail.raw`, so the commit seam
    // does not drop it. Boxed (off, default): fixed-height TailPanel on
    // placeholder rows at the flow tail (where thinking happens), so scroll
    // accounting treats it like content — reviewing history scrolls it away
    // with everything else and follow-bottom brings it back. The bordered
    // widget is overlaid only when the whole box fits inside the visible
    // window; a box cut by the window edge is left blank (it is one unit,
    // not clip-able rows). Terminal/fit guards are boxed-only — loose rows
    // are ordinary content and nothing squeezes. Panel body stays
    // UNPREFIXED (its title names the agent).
    let panel: Option<ThinkingPanelState> = if app.show_thoughts {
        if let Some(state) = app.thinking_panel_state() {
            let base = style_for(LineKind::Thought, theme);
            let agent_prefix = state
                .agent
                .as_ref()
                .map(|a| format!("[{a}] "))
                .unwrap_or_default();
            for (n, row) in state.lines.iter().enumerate() {
                // Attribution rides the first row only (tool-line convention).
                // Re-wrap just that row so the prefix cannot overflow the pane.
                let rows: Vec<String> = if n == 0 && !agent_prefix.is_empty() {
                    wrap(&format!("{agent_prefix}{row}"), app.transcript.wrap_width())
                } else {
                    vec![row.clone()]
                };
                for r in rows {
                    visual_map.push_unselectable(r.clone());
                    lines.push(Line::from(Span::styled(r, base)));
                }
            }
        }
        None
    } else {
        app.thinking_panel_state().filter(|_| {
            app.thinking_panel_wanted()
                && f.area().height >= MIN_PANEL_TERM_H
                && height >= (THINKING_PANEL_H + MIN_PANEL_HISTORY_ROWS) as usize
        })
    };
    let panel_flow_start = lines.len();
    if panel.is_some() {
        for _ in 0..THINKING_PANEL_H {
            visual_map.push_unselectable(String::new());
            lines.push(Line::default());
        }
    }

    // Mid-flow moves since the last frame — the plan block's re-paste splices
    // its old rows out and re-appends at the tail (批4 #11). The anchor below
    // was captured in the pre-splice numbering, so it translates through the
    // moves before resolving; holding raw indices would jump the reader by
    // the removed rows. A move that nets rows out must not trip the
    // tail-shrink hold either (`set_visible_moved`), or a following view
    // freezes with a blank gap under the re-pasted block.
    let moves = app.transcript.take_index_moves();

    // Scroll anchor: the output line currently at the window top (previous
    // frame's map) plus how far the head sits INSIDE that line's visual block.
    // `phi_tui::visual` owns the arithmetic: expanded thinking wraps one
    // OutputLine into dozens of rows, and the head routinely lands mid-block,
    // so resolving to the block's first row would fling the view a whole
    // block upward and undo every one-row scroll-down (session 20260923:
    // "can't scroll to the bottom after Ctrl+O"). Unselectable tail rows and
    // the first frame fall back to index-holding inside `set_visible_anchored`.
    let anchor = {
        let prev_total = app.visual_map.len();
        let prev_height = app.viewport.viewport_height.max(1);
        let prev_window = app.viewport.window_range(prev_total, prev_height);
        app.visual_map.anchor_at(prev_window.start)
    };

    // Visible window respecting scroll_offset and follow_bottom.
    let total = lines.len();
    let preferred_head = anchor.and_then(|a| visual_map.resolve_anchor(a.translate(&moves)));
    app.visual_map = visual_map;
    if moves.is_empty() {
        app.viewport
            .set_visible_anchored(total, height, preferred_head);
    } else {
        app.viewport
            .set_visible_moved(total, height, preferred_head);
    }
    let window = app.viewport.window_range(total, height);

    // The panel's screen rect = where its placeholder rows land in the window.
    let panel_rect = if panel.is_some()
        && window.start <= panel_flow_start
        && panel_flow_start + THINKING_PANEL_H as usize <= window.end
    {
        Some(Rect {
            x: area.x,
            y: area.y + (panel_flow_start - window.start) as u16,
            width: area.width,
            height: THINKING_PANEL_H,
        })
    } else {
        None
    };

    let visible: Vec<Line> = lines[window].to_vec();

    // No border/title — the transcript flows freely (Claude Code style); the
    // composer's own box is the visual boundary between output and input.
    f.render_widget(Paragraph::new(visible), area);

    if let (Some(rect), Some(state)) = (panel_rect, &panel) {
        f.render_widget(Clear, rect);
        render_thinking_panel(
            f,
            app,
            rect,
            &state.lines,
            state.agent.as_deref(),
            state.chars,
        );
    }
}
