//! Selection painting: the highlight covers exactly the dragged cells —
//! never the whole row, never half a wide glyph — and copy re-joins the
//! renderer's soft-wrap seams.

use super::super::*;
use crate::app::CharPoint;
use crossterm::event::{MouseButton, MouseEventKind};
use phi_tui::lines::OutputLine;

/// One rendered buffer cell as the tests see it.
struct CellView {
    sym: String,
    bg: Option<Color>,
}

/// The buffer row containing a needle: its screen `y`, the needle's first
/// cell `x0`, and every buffer column of that row. The row walk steps over a
/// wide glyph's continuation cell like the banner tests do.
struct RowView {
    y: usize,
    x0: usize,
    cells: Vec<CellView>,
}

fn row_cells_for(buf: &Buffer, width: usize, needle: &str) -> Option<RowView> {
    let area = buf.area;
    for y in 0..area.height as usize {
        let mut row = String::new();
        let mut col_of_byte: Vec<usize> = Vec::new();
        let mut x = 0usize;
        while x < width {
            let sym = buf.content()[y * width + x].symbol();
            for _ in 0..sym.len() {
                col_of_byte.push(x);
            }
            row.push_str(sym);
            x += sym.width().max(1);
        }
        if let Some(byte) = row.find(needle) {
            let cells = (0..width)
                .map(|cx| {
                    let c = &buf.content()[y * width + cx];
                    CellView {
                        sym: c.symbol().to_string(),
                        bg: c.style().bg,
                    }
                })
                .collect();
            return Some(RowView {
                y,
                x0: col_of_byte[byte],
                cells,
            });
        }
    }
    None
}

fn push_line(app: &mut App, text: &str) {
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        detail: None,
        text: text.into(),
        kind: LineKind::Normal,
        tool_state: None,
    });
}

/// protects: the highlight paints only the cells the drag selected — the
/// first row from the anchor column, the last row up to the head column —
/// while middle rows stay whole. fails_when: the bg bleeds over the whole
/// row to the pane edge (the old line-level paint). no_claim: the selection
/// color itself.
#[test]
fn selection_paints_only_the_dragged_cells() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    push_line(&mut app, "0123456789");
    push_line(&mut app, "abcdefghij");
    push_line(&mut app, "klmnopqrst");
    terminal.draw(|f| draw(f, &mut app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let first = row_cells_for(&buf, 80, "0123456789").expect("first row rendered");
    let third = row_cells_for(&buf, 80, "klmnopqrst").expect("third row rendered");
    let x0 = first.x0;
    // Drag from cell 3 of the first row to cell 6 of the third.
    app.handle_mouse(
        MouseEventKind::Down(MouseButton::Left),
        (x0 + 3) as u16,
        first.y as u16,
        80,
        24,
    );
    app.handle_mouse(
        MouseEventKind::Drag(MouseButton::Left),
        (x0 + 6) as u16,
        third.y as u16,
        80,
        24,
    );
    terminal.draw(|f| draw(f, &mut app)).unwrap();

    assert_eq!(app.selection_text(), "3456789\nabcdefghij\nklmnopq");
    let sel_bg = app.theme().selection_bg;
    let buf = terminal.backend().buffer().clone();
    let row0 = row_cells_for(&buf, 80, "0123456789").unwrap();
    let row2 = row_cells_for(&buf, 80, "klmnopqrst").unwrap();
    assert_ne!(
        row0.cells[x0 + 2].bg,
        Some(sel_bg),
        "cell before the anchor stays clear"
    );
    for cx in 3..10 {
        assert_eq!(
            row0.cells[x0 + cx].bg,
            Some(sel_bg),
            "first row: cell {cx} selected"
        );
    }
    for cx in 7..10 {
        assert_ne!(
            row2.cells[x0 + cx].bg,
            Some(sel_bg),
            "past the head: cell {cx} clear"
        );
    }
    for cx in 0..7 {
        assert_eq!(
            row2.cells[x0 + cx].bg,
            Some(sel_bg),
            "last row: cell {cx} selected"
        );
    }
}

/// protects: a wide CJK glyph is always whole in the selection — the drag
/// snaps to character edges, so copy is whole characters only and the
/// rendered glyphs stay intact (no half-glyph span split). fails_when: copy
/// cuts inside a double-width character, or the highlight reaches a glyph
/// the drag never touched. no_claim: the continuation cell's own style —
/// ratatui resets it and the terminal paints it from the glyph.
#[test]
fn wide_cjk_selection_snaps_whole_glyphs() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    push_line(&mut app, "你好世界");
    terminal.draw(|f| draw(f, &mut app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let row = row_cells_for(&buf, 80, "你好世界").expect("cjk row rendered");
    // Press on the CONTINUATION cell of 好 (cell 3) and release on the
    // continuation cell of 界 (cell 7): both snap out to whole glyphs.
    app.handle_mouse(
        MouseEventKind::Down(MouseButton::Left),
        (row.x0 + 3) as u16,
        row.y as u16,
        80,
        24,
    );
    app.handle_mouse(
        MouseEventKind::Drag(MouseButton::Left),
        (row.x0 + 7) as u16,
        row.y as u16,
        80,
        24,
    );
    terminal.draw(|f| draw(f, &mut app)).unwrap();

    assert_eq!(app.selection_text(), "好世界", "copy is whole characters");
    let sel_bg = app.theme().selection_bg;
    let buf = terminal.backend().buffer().clone();
    let row = row_cells_for(&buf, 80, "你好世界").unwrap();
    // Each glyph sits in one styled unit at its symbol cell (continuation
    // cells are ratatui's reset filler — the terminal paints them from the
    // glyph — so only the symbol cell carries the style to assert).
    for (cx, glyph, selected) in [
        (0, "你", false),
        (2, "好", true),
        (4, "世", true),
        (6, "界", true),
    ] {
        assert_eq!(
            row.cells[row.x0 + cx].sym,
            glyph,
            "glyph at cell {cx} intact"
        );
        if selected {
            assert_eq!(
                row.cells[row.x0 + cx].bg,
                Some(sel_bg),
                "glyph at cell {cx} selected"
            );
        } else {
            assert_ne!(
                row.cells[row.x0 + cx].bg,
                Some(sel_bg),
                "glyph at cell {cx} stays clear"
            );
        }
    }
    assert_ne!(
        row.cells[row.x0 + 8].bg,
        Some(sel_bg),
        "the pad past the text end stays clear"
    );
}

/// protects: copying every visual row of a soft-wrapped line re-joins it the
/// way it reads — the rendered seams put the break whitespace and run splits
/// back, so the copy equals the original line with no fabricated newlines.
/// fails_when: copy of a wrapped line contains `\n` at wrap points or glued
/// words. no_claim: the wrap width chosen for the pane.
#[test]
fn wrapped_copy_rejoins_rendered_seams() {
    let backend = TestBackend::new(24, 14);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    push_line(&mut app, "alpha beta gamma delta epsilon zeta");
    terminal.draw(|f| draw(f, &mut app)).unwrap();

    let total = app.visual_map.len();
    assert!(
        total > 1,
        "the line must wrap for this test to mean anything"
    );
    app.selection_state.select_range(
        CharPoint { row: 0, col: 0 },
        CharPoint {
            row: total - 1,
            col: usize::MAX,
        },
    );
    assert_eq!(app.selection_text(), "alpha beta gamma delta epsilon zeta");
}
