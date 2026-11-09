//! Picker line styling, theme slots, and framed bands.

use super::super::*;
use crossterm::event::{KeyCode, KeyModifiers};

#[test]
fn mention_lines_mark_kinds_and_elide_long_names() {
    let mut app = App::new();
    let root = std::env::temp_dir().join(format!("phimint-mention-lines-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("deep/nested/dir")).unwrap();
    std::fs::write(
        root.join("deep/nested/dir/a_very_long_test_file_name.rs"),
        "x",
    )
    .unwrap();
    app.set_workspace_root(root);

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    for c in "deep/nested/dir/a_very".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }

    let m = app.mention().expect("picker open");
    let rows = mention_lines(m, 30);
    let row_text =
        |i: usize| -> String { rows[i].spans.iter().map(|s| s.content.as_ref()).collect() };

    // Row 0 is the synthetic "use what I typed" row: the typed prefix itself
    // (21 columns) still fits the 26-column budget, so it is left whole. The
    // path is rendered with the platform separator (a PathBuf's own form), so
    // the expectation follows the OS rather than a hard-coded slash.
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        row_text(0),
        format!("» deep{sep}nested{sep}dir{sep}a_very"),
        "typed prefix echoed verbatim"
    );

    // Row 1 is the matching file: its 29-column name is elided into the
    // 25-column budget (30 − gutter 2 − marker 3).
    let file = row_text(1);
    assert!(file.starts_with("📄 "), "kind marker missing: {file:?}");
    assert!(file.ends_with("_name.rs"), "filename lost: {file:?}");
    assert!(file.contains("..."), "expected an elision: {file:?}");
    assert!(unicode_width::UnicodeWidthStr::width(file.as_str()) <= 28);
}

#[test]
fn slash_lines_elide_descriptions_to_the_band_width() {
    let mut app = App::new();
    app.set_skill_summaries(vec![(
        "review".into(),
        "Pre-landing PR review with a deliberately long description".into(),
    )]);
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);

    let s = app.slash().expect("picker open");
    let rows = slash_lines(s, 60, Theme::dark());
    let text: String = rows[0].spans.iter().map(|sp| sp.content.as_ref()).collect();
    assert!(text.starts_with("review"), "name column missing: {text:?}");
    assert!(text.ends_with("..."), "description not elided: {text:?}");
    assert!(unicode_width::UnicodeWidthStr::width(text.as_str()) <= 58);
}

#[test]
fn slash_lines_pads_the_name_column_in_display_columns() {
    let mut app = App::new();
    // Skill names come from free-form frontmatter `name:`, not a validated
    // ASCII slug, so a multi-byte name must not overshoot the name column.
    app.set_skill_summaries(vec![("代码审查工具".into(), "desc".into())]);
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);

    let s = app.slash().expect("picker open");
    let rows = slash_lines(s, 60, Theme::dark());
    // The name span alone must be exactly NAME_W (24) display columns. Padding
    // by char count would give 24 chars = 30 columns here.
    let name_span = &rows[0].spans[0];
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(name_span.content.as_ref()),
        24,
        "name column drifted: {:?}",
        name_span.content.as_ref()
    );
    // And the whole row still fits the band.
    let text: String = rows[0].spans.iter().map(|sp| sp.content.as_ref()).collect();
    assert!(
        unicode_width::UnicodeWidthStr::width(text.as_str()) <= 58,
        "row overflow: {text:?}"
    );
}

#[test]
fn slash_lines_fit_a_band_narrower_than_the_name_column() {
    let mut app = App::new();
    app.set_skill_summaries(vec![("review".into(), "a description".into())]);
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);

    let s = app.slash().expect("picker open");
    // A 20-column band is narrower than GUTTER_W + NAME_W + GAP_W (28). The
    // fixed 24-col name column built a 28-col row that `Paragraph` then
    // hard-clipped. Reachable via `ui.popup.width: 20`.
    let rows = slash_lines(s, 20, Theme::dark());
    let text: String = rows[0].spans.iter().map(|sp| sp.content.as_ref()).collect();
    assert!(
        unicode_width::UnicodeWidthStr::width(text.as_str()) <= 18,
        "row overflows a 20-col band: {text:?}"
    );
    // And a degenerate 3-column band must not go negative either.
    let tiny = slash_lines(s, 3, Theme::dark());
    let tiny_text: String = tiny[0].spans.iter().map(|sp| sp.content.as_ref()).collect();
    assert!(
        unicode_width::UnicodeWidthStr::width(tiny_text.as_str()) <= 1,
        "row overflows a 3-col band: {tiny_text:?}"
    );
}

#[test]
fn slash_lines_keeps_the_selected_row_two_tone() {
    let mut app = App::new();
    app.set_skill_summaries(vec![
        ("review".into(), "first".into()),
        ("qa".into(), "second".into()),
    ]);
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);

    let s = app.slash().expect("picker open");
    assert_eq!(s.selected_index(), 0, "expected the first row selected");
    let rows = slash_lines(s, 60, Theme::dark());
    // spans: [name, gap, desc]
    let fg = |row: usize, span: usize| rows[row].spans[span].style.fg;

    assert_eq!(fg(0, 0), Some(Color::White), "selected name lost its fg");
    // The description stays dim on the selected row, but lifts `faint` →
    // `lifted` so it survives the widget's selection background.
    assert_eq!(
        fg(0, 2),
        Some(Color::Gray),
        "selected description must not match the highlight background"
    );
    // Unselected row keeps the plain dim description.
    assert_eq!(
        fg(1, 2),
        Some(Theme::dark().faint),
        "unselected description drifted"
    );
    assert_eq!(fg(1, 0), Some(Color::White), "unselected name drifted");
}

#[test]
fn light_scheme_flips_the_gray_and_truecolor_slots() {
    // The Dark palette is pinned by the tests above (`App::new()` is Dark);
    // this guards the other half: a light terminal must not wear dark chrome.
    assert_eq!(
        style_for(LineKind::System, Theme::light()).fg,
        Some(Theme::light().muted)
    );
    assert_eq!(
        style_for(LineKind::ToolResult, Theme::light()).fg,
        Some(Theme::light().muted)
    );
    assert_eq!(
        style_for(LineKind::Thought, Theme::light()).fg,
        Some(Theme::light().thought)
    );
    assert_eq!(
        style_for(LineKind::User, Theme::light()).fg,
        Some(Theme::light().user)
    );
    // And the split is real: Light does not fall back to the Dark values.
    for kind in [
        LineKind::System,
        LineKind::ToolResult,
        LineKind::Thought,
        LineKind::User,
    ] {
        assert_ne!(
            style_for(kind, Theme::light()).fg,
            style_for(kind, Theme::dark()).fg,
            "{kind:?} is identical in Dark and Light"
        );
    }
}

#[test]
fn system_line_status_glyphs_take_semantic_hues() {
    // The product's own answer vocabulary: ✓ success, ✗ failure, ⚠ warning,
    // → next-step pointer — each glyph paints its hue, the body stays muted.
    for (text, hue) in [
        ("✓ my-tools installed · 2 tools", Color::Green),
        ("✗ connection refused", Color::Red),
        ("⚠ connections.json not saved", Color::Yellow),
        ("→ ask me to use them by name in chat", Theme::dark().strong),
    ] {
        let line = system_info_line(text, Theme::dark());
        assert_eq!(line.spans[0].style.fg, Some(hue), "glyph hue of {text:?}");
        assert_eq!(
            line.spans[1].style.fg,
            Some(Theme::dark().muted),
            "body of {text:?}"
        );
        // Byte-exact: selection and copy reconstruct the original line.
        let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(plain, text);
    }
}

#[test]
fn system_line_aligned_labels_read_strong() {
    // A command row: indent + label + double-space gap + description splits
    // like the `/` popup — the `/command` spec strong, the description muted.
    let text = "  /plugin add <path|url>   install a plugin";
    let line = system_info_line(text, Theme::dark());
    assert_eq!(line.spans.len(), 3);
    assert_eq!(line.spans[0].content.as_ref(), "  ");
    assert_eq!(line.spans[1].content.as_ref(), "/plugin add <path|url>");
    assert_eq!(
        line.spans[1].style.fg,
        Some(Theme::dark().strong),
        "command label must pop"
    );
    assert_eq!(
        line.spans[2].style.fg,
        Some(Theme::dark().muted),
        "description stays on the base"
    );
    let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(plain, text);

    // Tool-key rows in the /tools panel take the same split.
    let key_row = "      on   plugin.my-tools.deploy          trusted";
    let line = system_info_line(key_row, Theme::dark());
    assert_eq!(line.spans[1].content.as_ref(), "on");
    assert_eq!(line.spans[1].style.fg, Some(Theme::dark().strong));
}

#[test]
fn system_line_plain_text_paints_whole() {
    // Headers, markers and prose have no glyph and no column gap: one span on
    // the System base.
    for text in [
        "plugin commands:",
        "── resumed conversation ──",
        "tools — 12 total · 10 on · 2 off",
        // A double-space in prose past the label cap is not a column row.
        "  this label is definitely longer than thirty-two characters   and stays whole",
    ] {
        let line = system_info_line(text, Theme::dark());
        assert_eq!(line.spans.len(), 1, "expected one span for {text:?}");
        assert_eq!(
            line.spans[0].style.fg,
            Some(Theme::dark().muted),
            "base style for {text:?}"
        );
        assert_eq!(line.spans[0].content.as_ref(), text);
    }
}

#[test]
fn popup_highlight_follows_the_scheme() {
    let mut app = App::new();
    // `App::new()` is Dark: the highlight is the dark selection chrome.
    assert_eq!(
        app.popup_style().highlight.bg,
        Some(Theme::dark().selection_bg)
    );
    app.set_theme(Theme::light());
    assert_eq!(
        app.popup_style().highlight.bg,
        Some(Theme::light().selection_bg)
    );
    // Foreground stays the row's own — a forced fg would flatten the `/`
    // popup's two-tone name/description rows.
    assert_eq!(app.popup_style().highlight.fg, None);
}

/// Draw and return each row as `(symbol, fg, bg)` cells, so tests can pin
/// selection styling and not just the flattened text.
fn snapshot_cells(app: &mut App, width: u16, height: u16) -> Vec<Vec<(String, Color, Color)>> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("offscreen terminal");
    terminal.draw(|f| draw(f, app)).expect("offscreen draw");
    let buf = terminal.backend().buffer();
    let area = buf.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| {
                    let c = &buf[(x, y)];
                    (c.symbol().to_string(), c.fg, c.bg)
                })
                .collect()
        })
        .collect()
}

#[test]
fn empty_slash_shows_an_unselected_placeholder() {
    let mut app = App::new();
    app.set_skill_summaries(vec![
        ("review".into(), "first".into()),
        ("qa".into(), "second".into()),
    ]);
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    // A prefix that matches nothing leaves the picker open with zero rows.
    for c in "zzz".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert!(
        app.slash().is_some(),
        "picker must stay open on a dead prefix"
    );

    let rows = snapshot_cells(&mut app, 100, 40);
    let text_of =
        |r: &[(String, Color, Color)]| -> String { r.iter().map(|(s, _, _)| s.as_str()).collect() };
    let idx = rows
        .iter()
        .position(|r| text_of(r).contains("no matching skills"))
        .expect("placeholder row missing");
    let text = text_of(&rows[idx]);
    // The widget supplies the 2-column gutter, so the row pads itself by none —
    // with the old `"  no matching skills"` it rendered double-indented.
    assert!(
        text.starts_with("  no matching skills"),
        "placeholder gutter wrong: {text:?}"
    );
    // It is a notice, not a choice: no glyph of it may be highlighted.
    let highlighted: Vec<_> = rows[idx]
        .iter()
        .filter(|(sym, _, bg)| *bg == Color::DarkGray && !sym.trim().is_empty())
        .collect();
    assert!(
        highlighted.is_empty(),
        "placeholder highlighted as a selectable row: {highlighted:?}"
    );
}

#[test]
fn framed_style_draws_a_bordered_band() {
    use phi_tui::popup_list::PopupStyle;
    let mut app = App::new();
    let root = std::env::temp_dir().join(format!("phimint-render-framed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.rs"), "x").unwrap();
    app.set_workspace_root(root);
    app.set_popup_style(PopupStyle::framed(30, 9));

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('m'), KeyModifiers::NONE);

    let text = snapshot_text(&mut app, 100, 40);
    assert!(
        text.contains('╭') && text.contains('╰'),
        "border missing:\n{text}"
    );
    assert!(text.contains("📄 main.rs"), "file row missing:\n{text}");
    // `WidthSpec::Fixed(30)` must actually narrow the band: the top border row
    // is 30 columns (old popup width 64 — this is the assertion that fails
    // before the migration, so the test really pins the injection path).
    let border = text.lines().find(|l| l.contains('╭')).expect("border row");
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(border.trim_end()),
        30,
        "band width not applied:\n{text}"
    );
}

#[test]
fn framed_band_elides_rows_to_the_content_width() {
    use phi_tui::popup_list::PopupStyle;
    let mut app = App::new();
    let root = std::env::temp_dir().join(format!("phimint-framed-elide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a_very_long_test_file_name.rs"), "x").unwrap();
    app.set_workspace_root(root);
    // A 30-column framed band has only 28 columns of content — the border eats
    // two. Rows budgeted against the outer band would be clipped by `Paragraph`
    // at the right edge, losing the filename tail instead of marking the cut.
    app.set_popup_style(PopupStyle::framed(30, 9));

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);

    let text = snapshot_text(&mut app, 100, 40);
    let row = text.lines().find(|l| l.contains('📄')).expect("file row");
    // Strip the band's border cells so the assertions look at content only.
    // The popup is an overlay — process-line text behind the band's right edge
    // shares the row — so cut at the closing border instead of trimming.
    let inner = row.split('│').nth(1).unwrap_or(row).trim();
    // Budgeted at 28 content columns the row is ≤28 wide and the elision keeps
    // the tail whole. Budgeted at 30 it would render 30 columns into a 28-wide
    // area and come out as `...name.` — the `.rs` would be clipped off.
    assert!(
        inner.ends_with(".rs"),
        "filename tail clipped instead of elided: {inner:?}\n{text}"
    );
    assert!(
        inner.contains("..."),
        "expected an elision: {inner:?}\n{text}"
    );
    assert!(
        unicode_width::UnicodeWidthStr::width(inner) <= 28,
        "row wider than the 28-col content area: {inner:?}\n{text}"
    );
}
