//! Mouse selection and the context menu.
//!
//! Selection is character-precise (drag selects the cells dragged over),
//! double-click picks the word run, triple-click picks the logical line's
//! visual block, and copy re-joins soft-wrap seams — see
//! `phi_tui::selection` for the geometry and `notes/spec-tui-mouse-char-select.md`
//! for the contract these tests pin.

use super::super::*;
use crossterm::event::{MouseButton, MouseEventKind};
use phi_tui::visual::Seam;

/// A row-span selection `(row, col) ..= (row, col)` for the keyboard tests
/// that only need "a selection exists" to arm the copy paths.
fn sel(a: (usize, usize), b: (usize, usize)) -> Selection {
    Selection {
        anchor: CharPoint { row: a.0, col: a.1 },
        head: CharPoint { row: b.0, col: b.1 },
    }
}

#[test]
fn mouse_drag_selects_line_range() {
    let mut app = App::new();
    for i in 0..10 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            text: format!("line {i}"),
            kind: LineKind::Normal,
            detail: None,
            tool_state: None,
        });
    }
    app.output_area = Some((0, 0, 100, 10));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 2, 100, 10);
    app.handle_mouse(MouseEventKind::Drag(MouseButton::Left), 55, 5, 100, 10);
    assert_eq!(
        app.selection_state.selection,
        Some(Selection {
            anchor: CharPoint { row: 2, col: 0 },
            head: CharPoint { row: 5, col: 6 },
        })
    );
    assert_eq!(app.selection_text(), "line 2\nline 3\nline 4\nline 5");
}

#[test]
fn mouse_drag_up_normalizes_selection() {
    let mut app = App::new();
    for i in 0..10 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            text: format!("line {i}"),
            kind: LineKind::Normal,
            detail: None,
            tool_state: None,
        });
    }
    app.output_area = Some((0, 0, 100, 10));
    // Drag up-right: the anchor row is BELOW the head row; the normalized
    // copy runs from the earlier row's column to the later row's column.
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 55, 5, 100, 10);
    app.handle_mouse(MouseEventKind::Drag(MouseButton::Left), 0, 2, 100, 10);
    assert!(app.is_selected(3));
    assert_eq!(app.selection_text(), "line 2\nline 3\nline 4\nline 5");
}

#[test]
fn click_outside_output_clears_selection() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "x".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.output_area = Some((0, 0, 10, 5));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, 10, 5);
    assert!(app.selection_state.selection.is_some());
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 50, 10, 5); // below pane
    assert!(app.selection_state.selection.is_none());
}

#[test]
fn selection_text_clamps_stale_indices() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "a".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    // A head whose rows vanished extends to the end of the surviving row.
    app.selection_state.selection = Some(sel((0, 0), (5, 0)));
    assert_eq!(app.selection_text(), "a");
}

// ── character-precise selection (批4 #5, spec-tui-mouse-char-select.md) ──────

/// protects: a drag selects exactly the cells between the press and the
/// release — the first row from the anchor column, the last row to the head
/// column, middle rows whole. fails_when: whole rows copy again (the original
/// whole-line bug). no_claim: the highlight painting (render tests).
#[test]
fn mouse_drag_slices_at_char_precision() {
    let mut app = App::new();
    for _ in 0..3 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            text: "0123456789".into(),
            kind: LineKind::Normal,
            detail: None,
            tool_state: None,
        });
    }
    app.output_area = Some((0, 0, 100, 10));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 3, 0, 100, 10);
    app.handle_mouse(MouseEventKind::Drag(MouseButton::Left), 6, 2, 100, 10);
    assert_eq!(
        app.selection_text(),
        "3456789\n0123456789\n0123456",
        "first row cut at the anchor, last row cut at the head"
    );
}

/// protects: a double click selects the word run under the cursor, not the
/// line and not one character. fails_when: the pick leaks across the `.` or
/// grabs the whole line. no_claim: CJK dictionary segmentation (runs only).
#[test]
fn double_click_selects_the_word_run() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "foo.bar baz".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.output_area = Some((0, 0, 100, 5));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 2, 0, 100, 5);
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 2, 0, 100, 5);
    assert_eq!(app.selection_text(), "foo");
}

/// protects: a triple click selects the whole logical line — every visual row
/// of a soft-wrapped block, and copy re-joins the wrapped rows. fails_when:
/// only the clicked visual row is taken, or the join fabricates a newline.
/// no_claim: which width wrapped the line.
#[test]
fn triple_click_selects_the_wrapped_block() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "hello world tail".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "second".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    // Post-render state: one logical line wrapped into two visual rows.
    app.visual_map
        .push_seamed(0, "hello wo".to_string(), Seam::Break);
    app.visual_map
        .push_seamed(0, "rld tail".to_string(), Seam::Glue);
    app.visual_map.push_mapped(1, "second".to_string());
    app.viewport.set_visible(3, 10);
    app.output_area = Some((0, 0, 100, 10));
    for _ in 0..3 {
        app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 1, 1, 100, 10);
    }
    assert_eq!(app.selection_text(), "hello world tail");
}

/// protects: a drag right after a double/triple click starts a fresh
/// character-precise selection at the press point — the word pick does not
/// stick. fails_when: the drag extends the stale word selection. no_claim:
/// the click chain's timing window.
#[test]
fn drag_after_double_click_reanchors_at_press_point() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "foo bar".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.output_area = Some((0, 0, 100, 5));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, 100, 5);
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, 100, 5);
    assert_eq!(
        app.selection_text(),
        "foo",
        "the double click picked a word"
    );
    app.handle_mouse(MouseEventKind::Drag(MouseButton::Left), 5, 0, 100, 5);
    assert_eq!(
        app.selection_text(),
        "foo ba",
        "the drag re-anchored at the press point and cut at the head"
    );
}

/// protects: a drag pinned to the output pane's bottom edge row scrolls one
/// row per interval and extends the head onto the newly revealed row — even
/// with no further mouse events. fails_when: the view freezes at the edge, or
/// steps land faster than the interval. no_claim: the exact scroll step size.
#[test]
fn edge_autoscroll_scrolls_and_extends_head() {
    let mut app = App::new();
    for i in 0..10 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            text: format!("line {i}"),
            kind: LineKind::Normal,
            detail: None,
            tool_state: None,
        });
    }
    app.viewport.set_visible(10, 5);
    app.viewport.scroll_to_top(); // window 0..5, rows below the edge
    app.output_area = Some((0, 0, 100, 5));
    // Press and hold at the bottom edge row (y = 4 = ay + ah - 1).
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 2, 4, 100, 5);
    let t0 = Instant::now();
    assert!(app.edge_autoscroll_step(t0), "first step scrolls one row");
    assert_eq!(
        app.selection_state.selection,
        Some(Selection {
            anchor: CharPoint { row: 4, col: 2 },
            head: CharPoint { row: 5, col: 2 },
        }),
        "head extended onto the revealed row at the pointer's column"
    );
    assert!(
        !app.edge_autoscroll_step(t0 + Duration::from_millis(50)),
        "inside the interval: no step"
    );
    assert!(app.edge_autoscroll_step(t0 + Duration::from_millis(150)));
    assert_eq!(
        app.selection_state.selection.map(|s| s.head.row),
        Some(6),
        "one more row per interval"
    );
}

/// protects: copy re-joins a soft-wrapped block the way it reads — a wrap
/// that ate a space re-joins with a space, a wrap inside a run glues tight.
/// fails_when: a wrapped sentence copies with a fabricated `\n` or glued
/// words. no_claim: the renderer's wrap width (seams are pushed directly).
#[test]
fn selection_text_rejoins_soft_wrap_seams() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "hello worldwide".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.visual_map
        .push_seamed(0, "hello".to_string(), Seam::Break);
    app.visual_map
        .push_seamed(0, "world".to_string(), Seam::Space);
    app.visual_map
        .push_seamed(0, "wide".to_string(), Seam::Glue);
    app.viewport.set_visible(3, 10);
    app.output_area = Some((0, 0, 100, 10));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, 100, 10);
    app.handle_mouse(MouseEventKind::Drag(MouseButton::Left), 55, 2, 100, 10);
    assert_eq!(app.selection_text(), "hello worldwide");
}

// ── keyboard paths (unchanged behavior, new selection model) ────────────────

#[test]
fn ctrl_c_copies_selection_then_double_press_quits() {
    let mut app = App::new();
    // With selection → copy (caller clears selection after copy).
    app.selection_state.selection = Some(sel((0, 0), (5, 5)));
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::CopySelection)
    );
    // Caller clears selection after copy. First Ctrl+C → show hint.
    app.selection_state.selection = None;
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None
    );
    assert!(app.notice.as_deref().unwrap_or("").contains("Ctrl+C"));
    // Second Ctrl+C within timeout → quit.
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Quit)
    );
    // After timeout, hint resets: first press → hint again.
    app.quit_hint_at = Some(Instant::now() - std::time::Duration::from_secs(5));
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None
    );
    // While running → cancel (no hint).
    app.running = true;
    app.quit_hint_at = None;
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Cancel)
    );
}

#[test]
fn ctrl_c_force_quits_when_stuck_after_cancel() {
    let mut app = App::new();
    app.running = true;
    app.quit_hint_at = None;
    // First Ctrl+C while running -> cancel (and arms the force-quit window).
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Cancel)
    );
    assert!(app.notice.as_deref().unwrap_or("").contains("force quit"));
    // Second Ctrl+C within the window while still stuck -> force quit.
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Quit)
    );
    // After the window lapses (cancel landed but user pressed late), the
    // next press cancels again instead of quitting.
    app.running = true;
    app.quit_hint_at = Some(Instant::now() - std::time::Duration::from_secs(5));
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Cancel)
    );
}

#[test]
fn right_click_opens_menu_and_enter_copies() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "x".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.output_area = Some((0, 0, 10, 10));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, 20, 20);
    app.handle_mouse(MouseEventKind::Down(MouseButton::Right), 3, 4, 20, 20);
    assert_eq!(
        app.selection_state.context_menu,
        Some(ContextMenu {
            x: 3,
            y: 4,
            selected: 0
        })
    );
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Some(Action::CopySelection)
    );
    assert_eq!(app.selection_state.context_menu, None);
}

#[test]
fn context_menu_arrows_move_highlight_and_esc_closes() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "x".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.output_area = Some((0, 0, 10, 10));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, 20, 20);
    app.handle_mouse(MouseEventKind::Down(MouseButton::Right), 0, 0, 20, 20);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(
        app.selection_state.context_menu.as_ref().unwrap().selected,
        1
    );
    // Enter on the "cancel" item closes without copying.
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(app.selection_state.context_menu.is_none());
}

#[test]
fn clicking_menu_copy_item_copies_and_closes() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "x".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.output_area = Some((0, 0, 10, 10));
    app.selection_state.selection = Some(sel((0, 0), (0, 0)));
    // Open the menu at (0,0): a 12x4 box whose items sit at rows y+1 (copy)
    // and y+2 (dismiss).
    assert_eq!(
        app.handle_mouse(MouseEventKind::Down(MouseButton::Right), 0, 0, 20, 20),
        None
    );
    assert!(app.selection_state.context_menu.is_some());
    // Left-click the copy row -> copy, menu closes, selection preserved.
    assert_eq!(
        app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 1, 1, 20, 20),
        Some(Action::CopySelection)
    );
    assert!(app.selection_state.context_menu.is_none());
    assert!(app.selection_state.selection.is_some());
}

#[test]
fn clicking_menu_cancel_item_closes_without_copy() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "x".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.output_area = Some((0, 0, 10, 10));
    app.selection_state.selection = Some(sel((0, 0), (0, 0)));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Right), 0, 0, 20, 20);
    // The dismiss item is second, at row y+2. It closes the menu but keeps the
    // selection.
    assert_eq!(
        app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 1, 2, 20, 20),
        None
    );
    assert!(app.selection_state.context_menu.is_none());
    assert!(app.selection_state.selection.is_some());
}

#[test]
fn clicking_outside_menu_closes_it_and_restarts_selection() {
    let mut app = App::new();
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "x".into(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    app.output_area = Some((0, 0, 10, 10));
    app.selection_state.selection = Some(sel((0, 0), (0, 0)));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Right), 0, 0, 20, 20);
    assert!(app.selection_state.context_menu.is_some());
    // A click far outside the popup closes the menu (and starts a new
    // selection) exactly as before.
    assert_eq!(
        app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 15, 15, 20, 20),
        None
    );
    assert!(app.selection_state.context_menu.is_none());
}

#[test]
fn esc_clears_selection_before_composer() {
    let mut app = App::new();
    app.selection_state.selection = Some(sel((0, 0), (0, 2)));
    app.composer.insert_str("keep");
    assert_eq!(app.handle_key(KeyCode::Esc, KeyModifiers::NONE), None);
    assert!(app.selection_state.selection.is_none());
    assert_eq!(app.composer.text(), "keep");
}

#[test]
fn cmd_c_copies_selection_but_never_quits() {
    let mut app = App::new();
    // No selection → Cmd+C does nothing (it must never quit).
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::SUPER),
        None
    );
    // With a selection → copy.
    app.selection_state.selection = Some(sel((0, 0), (0, 0)));
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::SUPER),
        Some(Action::CopySelection)
    );
}

// ── Ctrl+C draft-clear (#6 follow-up): one press deals with the text ────────

#[test]
fn ctrl_c_clears_a_draft_before_any_quit_gesture() {
    let mut app = App::new();
    app.composer.insert_str("half-written");
    // First press clears the draft — a complete action, no quit window armed.
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None
    );
    assert_eq!(app.composer.text(), "");
    assert!(app.quit_hint_at.is_none());
    // The next press starts the normal quit gesture from scratch.
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None
    );
    assert!(app.notice.as_deref().unwrap_or("").contains("Ctrl+C"));
}

#[test]
fn ctrl_c_clear_resets_an_armed_quit_window() {
    // Rescue inserts text without a keystroke, so a stale force-quit window
    // can coexist with a draft; clearing must start a fresh gesture.
    let mut app = App::new();
    app.quit_hint_at = Some(Instant::now());
    app.composer.insert_str("rescued draft");
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None
    );
    assert_eq!(app.composer.text(), "");
    assert!(app.quit_hint_at.is_none());
}

#[test]
fn ctrl_c_cancel_keeps_a_draft_typed_while_running() {
    let mut app = App::new();
    app.running = true;
    app.composer.insert_str("steer draft");
    // Running keeps cancel at priority: the draft survives the cancel…
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Cancel)
    );
    assert_eq!(app.composer.text(), "steer draft");
    // …and the next press (now idle) clears it instead of drifting to quit.
    app.running = false;
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None
    );
    assert_eq!(app.composer.text(), "");
    assert!(app.quit_hint_at.is_none());
}

#[test]
fn ctrl_c_clear_precedes_background_task_cancel() {
    let mut app = App::new();
    app.background_tasks.insert(
        "bg1".to_string(),
        BackgroundTaskEntry {
            id: "bg1".to_string(),
            command: "sleep 100".to_string(),
            timeout_ms: 120_000,
            status: BackgroundTaskStatus::Running,
            started_at: Instant::now(),
            finished_at: None,
            reported: false,
            output_tail: String::new(),
            consumed: false,
        },
    );
    app.composer.insert_str("draft");
    // The draft is the first press; background tasks cancel on the second.
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None
    );
    assert_eq!(app.composer.text(), "");
    assert!(
        !app.notice
            .as_deref()
            .unwrap_or("")
            .contains("Background tasks")
    );
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None
    );
    assert!(
        app.notice
            .as_deref()
            .unwrap_or("")
            .contains("Background tasks cancelled")
    );
}
