//! Real terminal cursor anchoring for IME.

use super::super::*;
use phi_tui::lines::OutputLine;

// ---------------------------------------------------------------------------
// Real terminal cursor (IME anchoring)
//
// The composer used to paint its caret as a REVERSED span and leave the
// hardware cursor wherever the last diff cell landed. After a scroll redraw
// that is the status bar — and macOS Terminal.app draws IME marked text at
// the hardware cursor even when it is hidden, so the characters the user is
// typing showed up outside the input until Ctrl+L forced a full repaint.
// `draw` now parks the real cursor on the composer caret instead.
// ---------------------------------------------------------------------------

/// Screen cell of the composer's caret: `"> "` prefix + display width of the
/// text before the cursor. Panics if the composer band is not where the
/// layout says it is (single-row input, rules above and below).
fn caret_cell(app: &App, height: u16) -> (u16, u16) {
    let input_row = height - 3;
    let (line, byte) = app.composer.cursor();
    let text = app.composer.lines().get(line).cloned().unwrap_or_default();
    // Every composer row is indented two columns (`"> "` on the first).
    let mut w = 2u16;
    let mut b = 0usize;
    for ch in text.chars() {
        if b >= byte {
            break;
        }
        w += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        b += ch.len_utf8();
    }
    (w, input_row)
}

/// After every redraw the hardware cursor must sit on the composer caret —
/// including the redraw right after a scroll, which is where it used to be
/// left on the status bar.
#[test]
fn real_cursor_sits_on_composer_caret_across_scroll_and_typing() {
    let (w, h) = (100u16, 40u16);
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    for i in 0..120 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            detail: None,
            text: format!("history row {i} 一些中文内容"),
            kind: LineKind::Normal,
            tool_state: None,
        });
    }

    // Phase 1: fresh draw.
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_cursor_on_caret(&terminal, &app, h, "fresh draw");

    // Phase 2: scroll up out of follow-bottom — the status bar gains a scroll
    // counter, so the diff's last cell is on the status bar. This is the frame
    // that used to strand the pen there.
    for _ in 0..20 {
        app.scroll_wheel_up();
    }
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_cursor_on_caret(&terminal, &app, h, "after scroll up");

    // Phase 3: type CJK at the caret while still scrolled.
    app.composer.insert_str("在办完给");
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_cursor_on_caret(&terminal, &app, h, "after typing CJK scrolled");

    // Phase 4: move the caret back across the wide glyphs.
    app.composer.move_left();
    app.composer.move_left();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_cursor_on_caret(&terminal, &app, h, "after caret walk back");
}

fn assert_cursor_on_caret(terminal: &Terminal<TestBackend>, app: &App, h: u16, when: &str) {
    let backend = terminal.backend();
    assert!(
        backend.cursor_visible(),
        "{when}: the IME needs a visible hardware cursor"
    );
    let (x, y) = caret_cell(app, h);
    let pos = backend.cursor_position();
    assert_eq!(
        (pos.x, pos.y),
        (x, y),
        "{when}: cursor must sit on the composer caret"
    );
    // Explicit regression statement: never leave it on the status bar.
    assert_ne!(pos.y, h - 1, "{when}: cursor stranded on the status bar");
}

/// The caret column is display-width aware: on CJK each glyph is two columns.
#[test]
fn real_cursor_caret_column_counts_wide_glyphs() {
    let (w, h) = (80u16, 24u16);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut app = App::new();
    app.composer.insert_str("在办完给");
    app.composer.move_home();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let pos = terminal.backend().cursor_position();
    assert_eq!((pos.x, pos.y), (2, h - 3), "home is right after '> '");

    app.composer.move_right();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let pos = terminal.backend().cursor_position();
    assert_eq!(
        (pos.x, pos.y),
        (4, h - 3),
        "one wide glyph right is two columns"
    );

    app.composer.move_end();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let pos = terminal.backend().cursor_position();
    assert_eq!(
        (pos.x, pos.y),
        (2 + 8, h - 3),
        "end of four wide glyphs is eight columns"
    );
}

/// The fake REVERSED caret must be gone: a block cursor over a REVERSED cell
/// double-inverts and disappears, and the two would fight for the same cell.
#[test]
fn composer_caret_is_no_longer_a_reversed_span() {
    let (w, h) = (80u16, 24u16);
    let mut app = App::new();
    app.composer.insert_str("在办完给");
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let cells = buf.content();
    let width = buf.area.width as usize;
    let input_row = (h - 3) as usize;
    for x in 0..width {
        let cell = &cells[input_row * width + x];
        assert!(
            !cell.modifier.contains(Modifier::REVERSED),
            "composer row must not paint a fake caret at col {x}: {:?}",
            cell.symbol()
        );
    }
}
