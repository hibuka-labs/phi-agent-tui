//! Approval, ask, choice, context-menu, mention and slash popups.

use super::*;

/// The approval decision legend (tool prompts). The one line that must never be
/// clipped: it is pinned to the last row(s) of the popup, sized for, and
/// rendered apart from the message body.
///
/// BUG 4 (final form): letters only SELECT an option, bare Enter COMMITS it.
/// The footer is built from spans whose concatenated text equals
/// [`footer_legend`] for the prompt kind exactly ([`approval_footer_line`] — see
/// its tests), so the width budget stays honest while the selected option and
/// the commit key can be highlighted.
///
/// The words are product copy (T14, [`crate::shell::ApprovalCopy`]); the join ("   " between
/// options, "   -   " before the commit key) is layout, not wording.
const OPTION_GAP: &str = "   ";

const COMMIT_GAP: &str = "   -   ";

/// Which legend this prompt wears — see [`approval_footer_line`]. A product
/// consent gate wears the two-key legend (no standing "always"), tool
/// approvals the three-key one; the words come from the product's copy slots.
pub(super) fn footer_legend(copy: &crate::shell::ApprovalCopy, consent: bool) -> String {
    let mut legend = if consent {
        format!("{}{OPTION_GAP}{}", copy.consent_open, copy.consent_skip)
    } else {
        format!(
            "{}{OPTION_GAP}{}{OPTION_GAP}{}",
            copy.allow_once, copy.allow_always, copy.deny
        )
    };
    legend.push_str(COMMIT_GAP);
    legend.push_str(&copy.confirm);
    legend
}

/// The footer as a styled line: the selected option (and a live commit key when
/// one is selected) carry the highlight; the rest stay cyan. Concatenating the
/// spans yields [`footer_legend`] for the prompt kind verbatim.
pub(super) fn approval_footer_line(app: &App, consent: bool) -> Line<'static> {
    let copy = &app.copy.approval;
    let sel_bg = app.theme().selection_bg;
    let option = |text: &str, decision: ApprovalDecision| {
        if app.approval_selection.as_ref() == Some(&decision) {
            Span::styled(
                text.to_string(),
                Style::default().bg(sel_bg).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(text.to_string(), Style::default().fg(Color::Cyan))
        }
    };
    // Enter is inert without a selection — dimmed until it can commit.
    let commit = if app.approval_selection.is_some() {
        Span::styled(
            copy.confirm.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(copy.confirm.clone(), Style::default().fg(Color::DarkGray))
    };
    let gap = Span::styled(OPTION_GAP.to_string(), Style::default().fg(Color::Cyan));
    let choices: Vec<(&str, ApprovalDecision)> = if consent {
        vec![
            (copy.consent_open.as_str(), ApprovalDecision::AllowOnce),
            (copy.consent_skip.as_str(), ApprovalDecision::Deny),
        ]
    } else {
        vec![
            (copy.allow_once.as_str(), ApprovalDecision::AllowOnce),
            (copy.allow_always.as_str(), ApprovalDecision::AllowAlways),
            (copy.deny.as_str(), ApprovalDecision::Deny),
        ]
    };
    let mut spans = Vec::new();
    for (text, decision) in choices {
        if !spans.is_empty() {
            spans.push(gap.clone());
        }
        spans.push(option(text, decision.clone()));
    }
    spans.push(Span::styled(
        COMMIT_GAP.to_string(),
        Style::default().fg(Color::Cyan),
    ));
    spans.push(commit);
    Line::from(spans)
}

/// A centered modal popup showing the current approval request + the decision
/// legend.
///
/// BUG 1: the decision legend used to be the last line of one wrapped paragraph
/// — a long message pushed it past the bottom edge of the fixed 70%x50% box and
/// the user was shown an approval prompt with no keys to press. Now the box
/// sizes to its content (clamped to the terminal, which can still clip the
/// message tail — never the decisions), and the legend is rendered on its own
/// reserved footer rows.
pub(super) fn render_approval_popup(f: &mut Frame, app: &App) {
    let Some(request) = app.current_approval() else {
        return;
    };
    // One legend per prompt kind: a product consent gate wears the two-key
    // legend (no standing "always"), tool approvals the three-key one. Which
    // is which is product policy (ShellSpec `is_consent`); the words are
    // product copy (T14, `app.copy.approval`).
    let consent = app.is_consent_request(request);
    let copy = &app.copy.approval;
    let legend = footer_legend(copy, consent);

    let risk_color = risk_color(&request.risk_level);
    let risk_line = (copy.risk_line)(&request.risk_level);
    let mut head_lines = vec![
        Line::from(Span::styled(
            copy.title.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    // D4 source marker: a sub-agent's request is tagged on its own line; the main agent is unchanged.
    if let Some(source) = &request.source {
        head_lines.push(Line::from(Span::styled(
            (copy.requested_by)(source),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        head_lines.push(Line::from(""));
    }
    head_lines.push(Line::from(Span::styled(
        request.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    head_lines.push(Line::from(Span::styled(
        risk_line.clone(),
        Style::default().fg(risk_color),
    )));
    head_lines.push(Line::from(""));

    let message_lines: Vec<&str> = request.message.split('\n').collect();

    // Width from content: the footer is part of the budget so the legend fits
    // on a single row on any reasonable terminal (it wraps only where the
    // terminal is narrower than the legend itself).
    let content_w = [
        UnicodeWidthStr::width(copy.title.as_str()),
        request.source.as_deref().map_or(0, |s| {
            UnicodeWidthStr::width((copy.requested_by)(s).as_str())
        }),
        UnicodeWidthStr::width(request.title.as_str()),
        UnicodeWidthStr::width(risk_line.as_str()),
        UnicodeWidthStr::width(legend.as_str()),
    ]
    .into_iter()
    .chain(message_lines.iter().map(|s| UnicodeWidthStr::width(*s)))
    .max()
    .unwrap_or(0);

    let screen = f.area();
    // The area can never exceed the screen (a Rect taller than the buffer
    // panics the renderer), and the footer keeps at least one row for itself.
    let width = (content_w as u16 + 2)
        .min(screen.width.saturating_sub(2).max(3))
        .max(3)
        .min(screen.width.max(1));
    let inner_w = width.saturating_sub(2);
    let footer_rows = wrapped_rows(&legend, inner_w).max(1);
    // Head lines are short and counted unwrapped; message lines are budgeted
    // for the wrap width actually chosen above.
    let body_rows = head_lines
        .len()
        .saturating_add(
            message_lines
                .iter()
                .map(|s| wrapped_rows(s, inner_w) as usize)
                .sum::<usize>(),
        )
        .saturating_add(1) // blank line before the footer
        .min(u16::MAX as usize) as u16;
    let height = (body_rows.saturating_add(footer_rows).saturating_add(2))
        .min(screen.height.saturating_sub(2).max(3))
        .max(footer_rows + 3)
        .min(screen.height.max(1));
    let area = Rect::new(
        screen.width.saturating_sub(width) / 2,
        screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title("approval");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    // Footer last: it owns the bottom rows of the inner area unconditionally.
    let footer_h = footer_rows.min(inner.height);
    let footer_rect = Rect {
        y: inner.y + inner.height - footer_h,
        height: footer_h,
        ..inner
    };
    let body_rect = Rect {
        height: inner.height - footer_h,
        ..inner
    };

    let mut body = head_lines;
    body.extend(message_lines.iter().map(|s| Line::from(*s)));
    f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), body_rect);
    f.render_widget(
        Paragraph::new(approval_footer_line(app, consent)).wrap(Wrap { trim: true }),
        footer_rect,
    );
}

/// A centered modal popup for a pending `ask_user` question — the same window
/// language as the approval popup: the transcript stays visible underneath,
/// the box sizes to its content (clamped to the terminal), and the way out is
/// on reserved footer rows so a long question can never crowd it off screen.
///
/// The option list always ends in the "Chat about this" pseudo-row (the third
/// exit: type a question instead of picking), so the rows are never empty —
/// a 0-option host-call ask gets its window too (it used to hang invisible,
/// because [`draw_choice_popup`] draws nothing for zero rows). Selecting the
/// pseudo-row swaps in the clarify input face ([`draw_clarify_popup`]).
pub(super) fn render_ask_popup(f: &mut Frame, app: &App) {
    let Some(item) = app.current_ask() else {
        return;
    };
    if let Some(buffer) = &app.ask_clarify {
        draw_clarify_popup(
            f,
            &item.question,
            buffer,
            &app.copy.ask_clarify_footer,
            app.theme(),
        );
        return;
    }
    let mut rows = crate::picker::choice_rows(&item.options);
    rows.push(crate::picker::ChoiceRow {
        label: app.copy.ask_chat_row.clone(),
        note: Some(app.copy.ask_chat_note.clone()),
    });
    draw_choice_popup(f, &item.question, &rows, app.ask_selection, app.theme());
}

/// The choice popup's frame: question on top, option list under it, key legend
/// on reserved footer rows. Split from [`render_ask_popup`] so the layout can
/// be asserted without parking a real question on an [`App`].
///
/// An empty option list is a question nobody can answer — nothing is drawn.
pub(super) fn draw_choice_popup(
    f: &mut Frame,
    question: &str,
    rows: &[crate::picker::ChoiceRow],
    selection: usize,
    theme: Theme,
) {
    use crate::picker::{choice_footer, choice_lines, choice_list_height};

    if rows.is_empty() {
        return;
    }
    let screen = f.area();
    if screen.width < 3 || screen.height < 3 {
        return;
    }

    // Width from content — the legend is part of the budget so the way out
    // fits on one row on any reasonable terminal. Then the legend is re-picked
    // for the width actually chosen (the ladder sheds words as the box narrows).
    // The question pads 1 column each side, so it budgets those columns too —
    // without them the trailing character clips whenever the question is the
    // widest content in the box.
    let content_w = [
        UnicodeWidthStr::width(" Choose "),
        UnicodeWidthStr::width(question) + 2,
        UnicodeWidthStr::width(choice_footer(u16::MAX)),
    ]
    .into_iter()
    .chain(rows.iter().flat_map(|row| {
        let label = UnicodeWidthStr::width(row.label.as_str()) + 2; // cursor column
        let note = row
            .note
            .as_deref()
            .map_or(0, |note| UnicodeWidthStr::width(note) + 4); // indent
        [label, note]
    }))
    .max()
    .unwrap_or(0);
    let width = (content_w as u16 + 2)
        .min(screen.width.saturating_sub(2).max(3))
        .max(3)
        .min(screen.width.max(1));
    let inner_w = width.saturating_sub(2);

    let legend = choice_footer(inner_w);
    // Footer first and unconditionally (BUG 1's rule, from the approval
    // popup): the legend owns its rows even when the height budget shrinks.
    let footer_rows = wrapped_rows(legend, inner_w).max(1);
    // The question wraps at the padded text width, not the full inner width.
    let question_rows = wrapped_rows(question, inner_w.saturating_sub(2).max(1)).max(1);
    let list_rows = choice_list_height(rows).max(1);
    let height = (question_rows + list_rows + footer_rows + 2)
        .min(screen.height.saturating_sub(2).max(3))
        .max(footer_rows + 3)
        .min(screen.height.max(1));

    let area = Rect::new(
        screen.width.saturating_sub(width) / 2,
        screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        // #4: the ask box is a brand affordance — its border is the warm
        // yellow family, not plain chrome.
        .border_style(Style::default().fg(theme.prompt_border))
        .title(" Choose ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let footer_h = footer_rows.min(inner.height);
    let footer_rect = Rect {
        y: inner.y + inner.height - footer_h,
        height: footer_h,
        ..inner
    };
    let body_rect = Rect {
        height: inner.height - footer_h,
        ..inner
    };
    if body_rect.height == 0 {
        f.render_widget(
            Paragraph::new(Span::styled(legend, Style::default().fg(theme.faint)))
                .wrap(Wrap { trim: true }),
            footer_rect,
        );
        return;
    }

    // Squeeze priority, same as the old full-frame layout: the option list
    // and the legend hold their rows and the question absorbs the clamp. A
    // question that clips is still readable behind the window; a list that
    // clips hides options the user cannot see.
    let list_h = list_rows.min(body_rect.height);
    let q_h = body_rect.height - list_h;
    let question_rect = Rect {
        height: q_h,
        ..body_rect
    };
    let list_rect = Rect {
        y: body_rect.y + q_h,
        height: list_h,
        ..body_rect
    };

    if q_h > 0 {
        f.render_widget(
            Paragraph::new(question)
                .wrap(Wrap { trim: true })
                .block(Block::default().padding(Padding::horizontal(1))),
            question_rect,
        );
    }

    let selected = selection.min(rows.len().saturating_sub(1));
    let option_lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .flat_map(|(index, row)| choice_lines(row, index == selected, theme))
        .collect();
    f.render_widget(Paragraph::new(option_lines), list_rect);

    f.render_widget(
        Paragraph::new(Span::styled(legend, Style::default().fg(theme.faint)))
            .wrap(Wrap { trim: true }),
        footer_rect,
    );
}

/// The clarify input face: question on top, a one-line input under it (its
/// own buffer, cursor parked at the end), key legend on reserved footer rows.
/// Split from [`render_ask_popup`] so the layout can be asserted without
/// parking a real question on an [`App`].
pub(super) fn draw_clarify_popup(
    f: &mut Frame,
    question: &str,
    buffer: &str,
    footer: &str,
    theme: Theme,
) {
    let screen = f.area();
    if screen.width < 3 || screen.height < 3 {
        return;
    }

    // Width from content, same budget as the choice popup: the question pads
    // 1 column each side; the input row is "> " + text + cursor, padded too.
    let content_w = [
        UnicodeWidthStr::width(" Choose "),
        UnicodeWidthStr::width(question) + 2,
        UnicodeWidthStr::width(footer),
        UnicodeWidthStr::width("> ") + UnicodeWidthStr::width(buffer) + 3,
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    let width = (content_w as u16 + 2)
        .min(screen.width.saturating_sub(2).max(3))
        .max(3)
        .min(screen.width.max(1));
    let inner_w = width.saturating_sub(2);

    // Footer first and unconditionally (BUG 1's rule): the legend owns its
    // rows even when the height budget shrinks. The input row holds next; the
    // question absorbs the clamp — the place the user is typing must never
    // scroll out of its own window.
    let footer_rows = wrapped_rows(footer, inner_w).max(1);
    let question_rows = wrapped_rows(question, inner_w.saturating_sub(2).max(1)).max(1);
    let body_rows = question_rows + 2; // blank gap + the input row
    let height = (body_rows + footer_rows + 2)
        .min(screen.height.saturating_sub(2).max(3))
        .max(footer_rows + 3)
        .min(screen.height.max(1));

    let area = Rect::new(
        screen.width.saturating_sub(width) / 2,
        screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.prompt_border))
        .title(" Choose ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let footer_h = footer_rows.min(inner.height);
    let footer_rect = Rect {
        y: inner.y + inner.height - footer_h,
        height: footer_h,
        ..inner
    };
    let body_rect = Rect {
        height: inner.height - footer_h,
        ..inner
    };
    if body_rect.height == 0 {
        f.render_widget(
            Paragraph::new(Span::styled(footer, Style::default().fg(theme.faint)))
                .wrap(Wrap { trim: true }),
            footer_rect,
        );
        return;
    }

    let input_h = 1.min(body_rect.height);
    let gap_h = u16::from(body_rect.height > input_h);
    let q_h = body_rect.height - input_h - gap_h;
    let question_rect = Rect {
        height: q_h,
        ..body_rect
    };
    let input_rect = Rect {
        y: body_rect.y + q_h + gap_h,
        height: input_h,
        ..body_rect
    };

    if q_h > 0 {
        f.render_widget(
            Paragraph::new(question)
                .wrap(Wrap { trim: true })
                .block(Block::default().padding(Padding::horizontal(1))),
            question_rect,
        );
    }

    // The buffer is append-only, so the tail (where typing lands) is what
    // must stay visible — a long paste elides at the head.
    let input_budget = (inner_w as usize).saturating_sub(2 + 2 + 1).max(1); // padding + "> " + cursor
    let input = Line::from(vec![
        Span::styled("> ", Style::default().fg(theme.faint)),
        Span::raw(elide(buffer, input_budget, Elide::Tail { sep: None })),
        Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
    ]);
    f.render_widget(
        Paragraph::new(input).block(Block::default().padding(Padding::horizontal(1))),
        input_rect,
    );

    f.render_widget(
        Paragraph::new(Span::styled(footer, Style::default().fg(theme.faint)))
            .wrap(Wrap { trim: true }),
        footer_rect,
    );
}

/// The product pick modal (the `/model` chooser): a typed filter over the
/// product's rows. Same queue discipline as the ask popup — the front item
/// is the one on screen.
pub(super) fn render_pick_popup(f: &mut Frame, app: &App) {
    let Some(item) = app.current_pick() else {
        return;
    };
    let Some(picker) = &app.pick_filter else {
        return;
    };
    draw_pick_popup(
        f,
        PickFrame {
            title: &item.title,
            rows: &item.rows,
            visible: &picker.entries,
            filter: &picker.prefix,
            selection: picker.selected,
            footer: &app.copy.pick_footer,
            no_match: &app.copy.pick_no_match,
        },
        app.theme(),
    );
}

/// The pick modal's frame state — one conceptual view (rows + filter face +
/// chrome), so the draw function takes the view and the theme, not seven
/// loose pieces.
pub(super) struct PickFrame<'a> {
    pub(super) title: &'a str,
    pub(super) rows: &'a [crate::interaction::PickRow],
    /// Indices into `rows` (the filter's match list).
    pub(super) visible: &'a [usize],
    pub(super) filter: &'a str,
    /// Cursor into `visible`.
    pub(super) selection: usize,
    pub(super) footer: &'a str,
    pub(super) no_match: &'a str,
}

/// The pick modal's frame: filter row on top, the filtered rows under it
/// (the product's current row bold), key legend on reserved footer rows.
/// Split from [`render_pick_popup`] so the layout can be asserted without
/// parking a real pick on an [`App`].
///
/// [`PickFrame::visible`] indexes [`PickFrame::rows`] (the filter's match
/// list) and [`PickFrame::selection`] is a cursor into it — the filter owns
/// which rows show and where the cursor sits.
pub(super) fn draw_pick_popup(f: &mut Frame, frame: PickFrame<'_>, theme: Theme) {
    use crate::picker::{pick_lines, pick_list_height};
    let PickFrame {
        title,
        rows,
        visible,
        filter,
        selection,
        footer,
        no_match,
    } = frame;

    let screen = f.area();
    if screen.width < 3 || screen.height < 3 {
        return;
    }

    // Width from content — same budget as the choice popup, plus the filter
    // row ("> " + text + cursor, padded) and the no-match hint.
    let content_w = [
        UnicodeWidthStr::width(title) + 2,
        UnicodeWidthStr::width(footer),
        UnicodeWidthStr::width(no_match) + 2,
        UnicodeWidthStr::width("> ") + UnicodeWidthStr::width(filter) + 3,
    ]
    .into_iter()
    .chain(
        visible
            .iter()
            .filter_map(|index| rows.get(*index))
            .flat_map(|row| {
                let label = UnicodeWidthStr::width(row.label.as_str()) + 2; // cursor column
                let note = row
                    .note
                    .as_deref()
                    .map_or(0, |note| UnicodeWidthStr::width(note) + 4); // indent
                [label, note]
            }),
    )
    .max()
    .unwrap_or(0);
    let width = (content_w as u16 + 2)
        .min(screen.width.saturating_sub(2).max(3))
        .max(3)
        .min(screen.width.max(1));
    let inner_w = width.saturating_sub(2);

    // Footer first and unconditionally (BUG 1's rule): the legend owns its
    // rows even when the height budget shrinks. The filter row holds next —
    // the place the user is typing must never scroll out of its own window.
    // The list absorbs the clamp.
    let footer_rows = wrapped_rows(footer, inner_w).max(1);
    let list_content_rows = if visible.is_empty() {
        wrapped_rows(no_match, inner_w.saturating_sub(2).max(1)).max(1)
    } else {
        let picked: Vec<crate::interaction::PickRow> = visible
            .iter()
            .filter_map(|index| rows.get(*index).cloned())
            .collect();
        pick_list_height(&picked).max(1)
    };
    let height = (1 + list_content_rows + footer_rows + 2)
        .min(screen.height.saturating_sub(2).max(3))
        .max(footer_rows + 3)
        .min(screen.height.max(1));

    let area = Rect::new(
        screen.width.saturating_sub(width) / 2,
        screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.prompt_border))
        .title(format!(" {title} "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let footer_h = footer_rows.min(inner.height);
    let footer_rect = Rect {
        y: inner.y + inner.height - footer_h,
        height: footer_h,
        ..inner
    };
    let body_rect = Rect {
        height: inner.height - footer_h,
        ..inner
    };
    if body_rect.height == 0 {
        f.render_widget(
            Paragraph::new(Span::styled(footer, Style::default().fg(theme.faint)))
                .wrap(Wrap { trim: true }),
            footer_rect,
        );
        return;
    }

    let input_h = 1.min(body_rect.height);
    let input_rect = Rect {
        height: input_h,
        ..body_rect
    };
    let list_rect = Rect {
        y: body_rect.y + input_h,
        height: body_rect.height - input_h,
        ..body_rect
    };

    // The filter is append-only, so the tail (where typing lands) is what
    // must stay visible — a long paste elides at the head (clarify's face).
    let input_budget = (inner_w as usize).saturating_sub(2 + 2 + 1).max(1); // padding + "> " + cursor
    let input = Line::from(vec![
        Span::styled("> ", Style::default().fg(theme.faint)),
        Span::raw(elide(filter, input_budget, Elide::Tail { sep: None })),
        Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
    ]);
    f.render_widget(
        Paragraph::new(input).block(Block::default().padding(Padding::horizontal(1))),
        input_rect,
    );

    if visible.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(no_match, Style::default().fg(theme.faint)))
                .wrap(Wrap { trim: true })
                .block(Block::default().padding(Padding::horizontal(1))),
            list_rect,
        );
    } else {
        // Flatten the visible rows to display lines, remembering where each
        // candidate starts, so the window can follow the cursor: Enter must
        // never commit a row the user cannot see.
        let selected = selection.min(visible.len().saturating_sub(1));
        let mut all_lines: Vec<Line<'static>> = Vec::new();
        let mut starts: Vec<usize> = Vec::new();
        for (i, index) in visible.iter().enumerate() {
            starts.push(all_lines.len());
            if let Some(row) = rows.get(*index) {
                all_lines.extend(pick_lines(row, i == selected, theme));
            }
        }
        let total = all_lines.len();
        let cap = list_rect.height as usize;
        let sel_start = starts.get(selected).copied().unwrap_or(0);
        let sel_end = starts.get(selected + 1).copied().unwrap_or(total);
        let mut first = 0;
        if total > cap && sel_end > cap {
            first = (sel_end - cap).min(sel_start);
        }
        let window: Vec<Line<'static>> = all_lines.into_iter().skip(first).take(cap).collect();
        f.render_widget(Paragraph::new(window), list_rect);
    }

    f.render_widget(
        Paragraph::new(Span::styled(footer, Style::default().fg(theme.faint)))
            .wrap(Wrap { trim: true }),
        footer_rect,
    );
}

/// A small context menu at the right-click cell, with a highlighted "copy" /
/// "cancel" choice (keyboard-driven: Up/Down/Enter/Esc).
pub(super) fn render_context_menu(f: &mut Frame, app: &App) {
    let Some(menu) = app.context_menu() else {
        return;
    };

    let area = f.area();
    // Geometry shared with mouse hit-testing (App::handle_mouse): one source of
    // truth for where the popup actually lands on screen.
    let (x, y) = context_menu_pos(menu.x, menu.y, area.width, area.height);
    let rect = Rect::new(x, y, CONTEXT_MENU_W, CONTEXT_MENU_H);
    f.render_widget(Clear, rect);

    let items = [" Copy ", " Cancel "];
    let sel_bg = app.theme().selection_bg;
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let style = if i == menu.selected {
                Style::default().bg(sel_bg).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(label.to_string(), style))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title("copy");
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Rows for the `@` popup: kind marker + name, elided to the band width.
/// `width` is the row-content column count
/// (`PopupStyle::content_width(band_width(…))`), not the outer band.
pub(super) fn mention_lines(m: &MentionCompleter, width: usize) -> Vec<Line<'static>> {
    m.entries()
        .iter()
        .map(|e| {
            let marker = if e.synthetic {
                "» "
            } else if e.is_dir {
                "📁 "
            } else {
                "📄 "
            };
            let budget = width.saturating_sub(GUTTER_W + UnicodeWidthStr::width(marker));
            let text = elide(&e.name, budget, Elide::Tail { sep: Some('/') });
            Line::from(Span::raw(format!("{marker}{text}")))
        })
        .collect()
}

/// Rows for the `/` popup: name column + dim description, elided to the band
/// width (`Head` for the description: it reads front-to-back).
///
/// The row is deliberately two-tone on the selected line as well: the widget's
/// `highlight` carries only the background + emphasis, so both foregrounds stay
/// the product's call. The description steps `faint` → `lifted` when selected
/// so it survives the highlight's background instead of vanishing into it.
pub(super) fn slash_lines(s: &SlashCompleter, width: usize, theme: Theme) -> Vec<Line<'static>> {
    const NAME_W: usize = 24;
    const GAP_W: usize = 2;
    let selected = s.selected_index();
    s.entries()
        .iter()
        .enumerate()
        .map(|(i, (name, desc))| {
            // Fit the fixed columns inside the band. A band narrower than
            // GUTTER_W + NAME_W + GAP_W would otherwise build a row wider than
            // the band and have `Paragraph` hard-clip the tail — reachable now
            // that `ui.popup.width` accepts any column count. Shrinking the
            // name column first keeps the row exactly `width` wide at any size.
            let avail = width.saturating_sub(GUTTER_W);
            let name_w = NAME_W.min(avail);
            let gap_w = GAP_W.min(avail.saturating_sub(name_w));
            let budget = avail.saturating_sub(name_w + gap_w);

            let name = elide(name, name_w, Elide::Tail { sep: None });
            let desc_fg = if i == selected {
                theme.lifted
            } else {
                theme.faint
            };
            Line::from(vec![
                Span::styled(pad_cols(&name, name_w), Style::default().fg(theme.strong)),
                Span::raw(" ".repeat(gap_w)),
                Span::styled(
                    elide(desc, budget, Elide::Head),
                    Style::default().fg(desc_fg),
                ),
            ])
        })
        .collect()
}

/// The `@` mention popup: the product's rows rendered by the popup-list widget.
pub(super) fn render_mention_popup(f: &mut Frame, app: &App, composer: Rect) {
    let Some(m) = app.mention() else {
        return;
    };
    let style = app.popup_style();
    // Budget the rows against the content width, not the band: a framed band
    // spends 2 columns on its border, and `Paragraph` would hard-clip those.
    let width = style.content_width(band_width(composer, f.area(), &style.width)) as usize;
    PopupList {
        rows: mention_lines(m, width),
        selected: m.selected_index(),
        style,
        title: None,
    }
    .render_at(f, composer);
}

/// The `/` skill popup: the product's rows rendered by the popup-list widget.
pub(super) fn render_slash_popup(f: &mut Frame, app: &App, composer: Rect) {
    let Some(s) = app.slash() else {
        return;
    };
    let style = app.popup_style();
    // Budget the rows against the content width, not the band: a framed band
    // spends 2 columns on its border, and `Paragraph` would hard-clip those.
    let width = style.content_width(band_width(composer, f.area(), &style.width)) as usize;
    let mut rows = slash_lines(s, width, app.theme());
    // An empty result is not a choice: show it as plain dim text that is never
    // highlighted. The widget already supplies the 2-column gutter, so the row
    // carries no padding of its own. `usize::MAX` never equals `window.start +
    // n`, so no row can be marked selected.
    let selected = if rows.is_empty() {
        rows.push(Line::from(Span::styled(
            "no matching skills",
            Style::default().fg(app.theme().faint),
        )));
        usize::MAX
    } else {
        s.selected_index()
    };
    PopupList {
        rows,
        selected,
        style,
        title: None,
    }
    .render_at(f, composer);
}

/// Severity color for the risk line (T14: the WORDS are product copy via
/// `ApprovalCopy::risk_line`; the color is visual mechanism, keyed on level).
pub(super) fn risk_color(level: &RiskLevel) -> Color {
    match level {
        RiskLevel::Safe => Color::Green,
        RiskLevel::Sensitive => Color::Yellow,
        RiskLevel::Destructive => Color::Red,
    }
}
