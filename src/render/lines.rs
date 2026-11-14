//! Line builders: tool lines, system lines, span styling.

use super::*;

use phi_tui::selection::slice_cells;

pub(super) fn style_for(kind: LineKind, theme: Theme) -> Style {
    // Hue is punctuation, grey is area (design §5). Only three things should
    // jump out of a screen: your input (blue), what went wrong (red), what is
    // running right now (cyan). Tools and results are the densest elements in
    // the transcript — tint those as a block and the pane turns into noise.
    // Grays and absolute tones come from `theme` so the same roles survive a
    // light background; the hues above are the terminal's to paint.
    match kind {
        LineKind::Normal => Style::default(),
        // Process text: dimmer than results, still legible. Shape still
        // separates it from results (summary line / un-indented body vs
        // `  < ` + indented payload).
        LineKind::Thought => Style::default().fg(theme.thought),
        LineKind::Plan => Style::default().fg(Color::Cyan),
        LineKind::Tool => Style::default().fg(Color::Cyan),
        LineKind::Done => Style::default().fg(Color::Green),
        // Result text is the tool's answer, not a verdict — grey, so a red or
        // green actually means something.
        LineKind::ToolResult => Style::default().fg(theme.muted),
        LineKind::Error => Style::default().fg(Color::Red),
        // System messages are the product talking back — command answers,
        // notices, verdicts — content the user asked for or must see, so they
        // read at the `muted` level (tool arguments, result bodies), not the
        // `faint` chrome slot. The composition in [`system_info_line`] adds
        // hue and label emphasis on top for the shapes worth scanning.
        LineKind::System => Style::default().fg(theme.muted),
        // A cancel is a settled fact, not an alarm — the user asked for it.
        // Grey keeps red meaning "something broke" and the marker still
        // records that a real mid-run interruption happened.
        LineKind::Cancelled => Style::default().fg(theme.muted),
        LineKind::Approval => Style::default().fg(Color::Yellow),
        // Attention, not failure: amber says "look here", red says "this
        // broke". A waiting-for-input notice is the first kind, not the
        // second.
        LineKind::Notice => Style::default().fg(Color::Yellow),
        LineKind::User => Style::default().fg(theme.user).add_modifier(Modifier::BOLD),
    }
}

/// Split a tool invocation body into `(who, args)`.
///
/// `runtime::tool_invocation_text` joins the tool name (plus any sub-agent
/// prefix) to its readable arguments with **two** spaces, and emits the name
/// alone when there is nothing worth naming. Two spaces is the contract — a
/// tool name never contains a double space, so the split is unambiguous.
pub(super) fn split_tool_body(body: &str) -> (&str, &str) {
    body.split_once("  ").unwrap_or((body, ""))
}

/// Style for a tool state's leading glyph — the one row element whose meaning
/// is "how is this call doing", so it gets the state colour.
pub(super) fn tool_state_style(state: ToolState, theme: Theme) -> Style {
    match state {
        ToolState::Running => Style::default().fg(Color::Cyan),
        ToolState::Done => Style::default().fg(Color::LightGreen),
        ToolState::Failed | ToolState::Denied => Style::default().fg(Color::Red),
        ToolState::Queued => Style::default().fg(theme.faint),
    }
}

/// A tool invocation line, composed rather than uniformly tinted.
///
/// The `*` in `text` is a marker slot, not a glyph: it is replaced here by the
/// call's state glyph (the braille frame while running, `o`/`x` once settled),
/// so the row answers "did this finish?" without opening anything. Arguments
/// drop to grey — the tool name is the identity, the values are detail.
pub(super) fn tool_invocation_line(
    text: &str,
    state: ToolState,
    spinner: &str,
    theme: Theme,
) -> Line<'static> {
    let body = text.strip_prefix("* ").unwrap_or(text);
    let (who, args) = split_tool_body(body);
    let glyph = match state {
        ToolState::Running => spinner,
        ToolState::Done => "o",
        ToolState::Queued => "o",
        ToolState::Failed | ToolState::Denied => "x",
    };
    let mut spans = vec![
        Span::styled(glyph.to_string(), tool_state_style(state, theme)),
        Span::styled(format!(" {who}"), style_for(LineKind::Tool, theme)),
    ];
    if !args.is_empty() {
        spans.push(Span::styled(
            format!("  {args}"),
            Style::default().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

/// Row indent for a folded block's payload.
///
/// Four columns, which is exactly where a result row's answer starts (`  < `
/// is the connector) — so the extra rows read as more of the same answer
/// rather than as a quote. Under a call row the same indent still nests
/// cleanly, and one constant keeps the ladder's shape identical everywhere.
pub(super) const FOLD_INDENT: &str = "    ";

/// The meta row for a tool result: the connector dimmer than the answer.
///
/// `  < ` is the result's attachment marker (the glyph column plus one). It is
/// punctuation, so it sits in the darkest grey; the tool's words are the
/// content and take the result style.
pub(super) fn result_meta_line(text: &str, kind: LineKind, theme: Theme) -> Line<'static> {
    let connector = Style::default().fg(theme.faint);
    if let Some(rest) = text.strip_prefix("  < ") {
        Line::from(vec![
            Span::styled("  < ".to_string(), connector),
            Span::styled(rest.to_string(), style_for(kind, theme)),
        ])
    } else if text.trim_end() == "  <" {
        Line::from(Span::styled(text.to_string(), connector))
    } else {
        // Denied / error rows keep their own wording and colour.
        Line::from(Span::styled(text.to_string(), style_for(kind, theme)))
    }
}

/// Hue for a leading status glyph on a system line — the same semantic hues
/// the transcript already uses for Done / Error / Approval, so a success or
/// failure inside a command answer reads at a glance.
///
/// cjk-glyph-exempt: begin — this selector only recolours glyphs the product
/// already wrote into the message; it computes no columns, so the CJK
/// double-width disagreement can't shift any layout.
pub(super) fn system_glyph_style(glyph: char, theme: Theme) -> Option<Style> {
    match glyph {
        '✓' => Some(Style::default().fg(Color::Green)),
        '✗' => Some(Style::default().fg(Color::Red)),
        '⚠' => Some(Style::default().fg(Color::Yellow)),
        '●' | '⚙' | '→' => Some(Style::default().fg(theme.strong)),
        _ => None,
    }
}

// cjk-glyph-exempt: end

/// Longest column-aligned label worth highlighting. The product's own panels
/// align command and tool-key columns at 32; anything longer is prose that
/// happened to contain a double space, and stays on the base style.
const SYSTEM_LABEL_MAX: usize = 32;

/// A system line, composed rather than uniformly tinted: command answers are
/// the product talking back, and the shapes worth scanning get structure.
///
/// Three rules, all byte-exact partitions of `text` (selection and copy see
/// the original):
///
/// 1. A leading status glyph (`✓ ✗ ⚠ ● ⚙ →`) takes its hue; the rest of the
///    line stays on the System base.
/// 2. A column-aligned row — indent, a short label, a double-space gap, a
///    description — splits like the `/` popup's name/description: the label
///    (a `/command` spec or a tool key) reads strong, the description muted.
/// 3. Everything else (headers, markers, wrapped prose) paints whole.
pub(super) fn system_info_line(text: &str, theme: Theme) -> Line<'static> {
    let base = style_for(LineKind::System, theme);
    if let Some(g) = text.chars().next()
        && let Some(gs) = system_glyph_style(g, theme)
    {
        return Line::from(vec![
            Span::styled(g.to_string(), gs),
            Span::styled(text[g.len_utf8()..].to_string(), base),
        ]);
    }
    let trimmed = text.trim_start();
    let indent = text.len() - trimmed.len();
    if indent > 0
        && let Some(split_at) = trimmed.find("  ")
    {
        let (label, desc) = trimmed.split_at(split_at);
        let key = label.trim_end();
        if !key.is_empty() && key.chars().count() <= SYSTEM_LABEL_MAX && !desc.trim().is_empty() {
            return Line::from(vec![
                Span::styled(text[..indent].to_string(), base),
                Span::styled(key.to_string(), Style::default().fg(theme.strong)),
                Span::styled(desc.to_string(), base),
            ]);
        }
    }
    Line::from(Span::styled(text.to_string(), base))
}

/// Build the styled line for `text`, with styled byte-runs overriding the base
/// style. Segments outside the runs keep `base`; each run takes `base` patched
/// with its banner palette color (so a selection background survives).
pub(super) fn span_line(
    text: &str,
    spans: &[SpanSpec],
    base: Style,
    theme: Theme,
) -> Line<'static> {
    if spans.is_empty() {
        return Line::from(Span::styled(text.to_string(), base));
    }
    let mut out = Vec::with_capacity(spans.len() * 2 + 1);
    let mut cur = 0usize;
    for SpanSpec { start, len, style } in spans {
        let start = *start;
        let end = start + *len;
        if start > cur {
            out.push(Span::styled(text[cur..start].to_string(), base));
        }
        out.push(Span::styled(
            text[start..end].to_string(),
            base.patch((*style).resolve(theme)),
        ));
        cur = end;
    }
    if cur < text.len() {
        out.push(Span::styled(text[cur..].to_string(), base));
    }
    Line::from(out)
}

/// Paint `bg` over just the display cells `[lo, hi)` of `line` — the
/// character-selection interval. Spans split at the interval edges (wide
/// characters snap whole, so no glyph is ever half painted); `hi` past the
/// row's text end paints to that end. The line-level style stays untouched:
/// the highlight covers the selected text exactly, not the whole row.
pub(super) fn paint_bg(line: &mut Line<'_>, bg: Color, lo: usize, hi: usize) {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 2);
    let mut cell = 0usize;
    for span in &line.spans {
        let w = span.content.as_ref().width();
        let (s, e) = (cell, cell + w);
        cell = e;
        if e <= lo || s >= hi {
            out.push(Span::styled(span.content.to_string(), span.style));
            continue;
        }
        if s < lo {
            out.push(Span::styled(
                slice_cells(&span.content, 0, lo - s),
                span.style,
            ));
        }
        out.push(Span::styled(
            slice_cells(&span.content, lo.saturating_sub(s), hi.min(e) - s),
            span.style.bg(bg),
        ));
        if e > hi {
            out.push(Span::styled(
                slice_cells(&span.content, hi - s, w),
                span.style,
            ));
        }
    }
    line.spans = out;
}

/// Paint the character-precision selection background on one visual row: the
/// selected cell interval of the row's plain text, if any (see
/// [`crate::app::App::selected_cols`]).
pub(super) fn apply_selection(line: &mut Line<'static>, app: &App, vis_idx: usize) {
    let plain = line_plain_text(line);
    if let Some((lo, hi)) = app.selected_cols(vis_idx, &plain) {
        paint_bg(line, app.theme().selection_bg, lo, hi);
    }
}
