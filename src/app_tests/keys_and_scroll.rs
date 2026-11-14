//! Global key handling and viewport scrolling.

use super::super::*;
use super::common::*;

#[test]
fn shift_enter_inserts_newline_not_submit() {
    let mut app = App::new();
    app.composer.insert_str("a");
    let r = app.handle_key(KeyCode::Enter, KeyModifiers::SHIFT);
    assert_eq!(r, None);
    assert_eq!(app.composer.text(), "a\n");
}

/// A terminal that reports the Backspace/Delete key as BS (0x08) — xterm's
/// default, and what many Linux setups send — reaches crossterm as
/// `Char('h') + Ctrl`. Without a binding the composer TYPES a literal 'h'
/// per press instead of deleting ("hh" where the user asked for a delete).
#[test]
fn ctrl_h_deletes_backward_instead_of_typing_h() {
    let mut app = App::new();
    app.composer.insert_str("ab");
    assert_eq!(
        app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL),
        None
    );
    assert_eq!(app.composer.text(), "a");
}

/// Any other Ctrl+<letter> chord is not text either: crossterm turns control
/// bytes (0x01–0x1A) into `Char(letter) + Ctrl`, so an unhandled chord would
/// type a letter into the composer instead of doing what the terminal means.
#[test]
fn ctrl_letter_chords_never_type_into_the_composer() {
    let mut app = App::new();
    app.handle_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    app.handle_key(KeyCode::Char('k'), KeyModifiers::CONTROL);
    app.handle_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(app.composer.text(), "");
}

#[test]
fn ctrl_c_cancels_when_running() {
    let mut app = App::new();
    app.running = false;
    // First Ctrl+C when idle → show hint (not quit yet).
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None
    );
    // Second Ctrl+C within timeout → quit.
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Quit)
    );
    // While running → cancel immediately (no hint).
    app.running = true;
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Cancel)
    );
}

#[test]
fn ctrl_l_requests_full_repaint() {
    let mut app = App::new();
    // Plain 'l' still types a literal character.
    assert_eq!(app.handle_key(KeyCode::Char('l'), KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "l");
    // Ctrl+L is the screen-repair key: surfaces the redraw action, never types.
    app.composer.clear();
    assert_eq!(
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL),
        Some(Action::Redraw)
    );
    // Global: context routing (here an open approval popup) must not swallow
    // it — screen repair has to work even mid-approval.
    pending_approval(&mut app);
    assert_eq!(
        app.handle_key(KeyCode::Char('l'), KeyModifiers::CONTROL),
        Some(Action::Redraw)
    );
}

/// protects: the paste key fires on both Ctrl+V and Cmd+V (where the
/// terminal forwards it) and a bare 'v' still types a character.
/// fails_when: a forwarded Cmd+V types a stray 'v' instead of pasting, or
/// either combo stops reaching the paste pipeline. no_claim: which combo
/// any given terminal delivers to the app.
#[test]
fn v_with_ctrl_or_super_reaches_the_paste_pipeline() {
    let mut app = App::new();
    assert_eq!(app.handle_key(KeyCode::Char('v'), KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "v", "bare v still types");
    app.composer.clear();
    assert_eq!(
        app.handle_key(KeyCode::Char('v'), KeyModifiers::CONTROL),
        Some(Action::PasteImage)
    );
    assert_eq!(
        app.handle_key(KeyCode::Char('v'), KeyModifiers::SUPER),
        Some(Action::PasteImage)
    );
}

#[test]
fn ctrl_b_reopens_the_canvas_even_mid_approval() {
    // Row 16: the crash face advertises "ctrl+b reopen", and the consent prompt
    // of the canvas that just died is exactly the popup that would otherwise
    // swallow the key. A chord can never be an approval decision, so it must
    // route globally like Ctrl+L.
    let mut app = App::new();
    assert_eq!(
        app.handle_key(KeyCode::Char('b'), KeyModifiers::CONTROL),
        Some(Action::CanvasReopen)
    );
    // Plain 'b' types — only the chord reopens.
    assert_eq!(app.handle_key(KeyCode::Char('b'), KeyModifiers::NONE), None);
    assert!(app.composer.text().contains('b'));
    app.composer.clear();

    pending_approval(&mut app);
    assert_eq!(
        app.handle_key(KeyCode::Char('b'), KeyModifiers::CONTROL),
        Some(Action::CanvasReopen),
        "an approval popup must not swallow the recovery key it advertises"
    );
    assert!(
        app.has_pending_approval(),
        "and it must not decide the prompt"
    );
    assert_eq!(app.approval_selection, None);
}

#[test]
fn turn_error_appends_red_line_and_resets() {
    let mut app = App::new();
    app.running = true;
    app.handle_event(TuiEvent::TurnError("boom".to_string()));
    assert!(!app.running);
    assert_eq!(app.status, AgentStatus::Idle);
    assert_eq!(app.transcript.output.last().unwrap().kind, LineKind::Error);
    assert!(app.transcript.output.last().unwrap().text.contains("boom"));
}

#[test]
fn streaming_tail_lines_wraps_long_text() {
    let mut app = App::new();
    // 250 chars at DEFAULT_WRAP_WIDTH=100 → three wrapped lines.
    let long = "a".repeat(250);
    app.handle_event(TuiEvent::Runtime(text(&long)));
    let (lines, kind) = app.streaming_tail_lines().expect("tail present");
    assert_eq!(
        lines,
        ["a".repeat(100), "a".repeat(100), "a".repeat(50)].as_slice()
    );
    assert_eq!(kind, LineKind::Normal);
}

#[test]
fn streaming_tail_exposes_uncommitted_text() {
    let mut app = App::new();
    assert_eq!(app.streaming_tail_lines(), None);
    app.handle_event(TuiEvent::Runtime(text("hel")));
    app.handle_event(TuiEvent::Runtime(text("lo")));
    let (lines, kind) = app.streaming_tail_lines().expect("tail present");
    assert_eq!(lines, ["hello".to_string()].as_slice());
    assert_eq!(kind, LineKind::Normal);
    // A structural event flushes the tail into committed output (and, for a
    // tool call, also appends an inline invocation line).
    app.handle_event(TuiEvent::Runtime(tool_started("build")));
    assert_eq!(app.streaming_tail_lines(), None);
    assert_eq!(app.transcript.output[0].text, "hello");
    assert_eq!(app.transcript.output[1].kind, LineKind::Tool);
}

#[test]
fn streaming_tail_flips_to_text_after_thought() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(thought("hmm")));
    let (lines, kind) = app.streaming_tail_lines().expect("thought tail present");
    assert_eq!(lines, ["hmm".to_string()].as_slice());
    assert_eq!(kind, LineKind::Thought);
    app.handle_event(TuiEvent::Runtime(text("answer")));
    let (lines, kind) = app.streaming_tail_lines().expect("text tail present");
    assert_eq!(lines, ["answer".to_string()].as_slice());
    assert_eq!(kind, LineKind::Normal);
}

#[test]
fn scroll_up_steps_from_bottom_not_noop() {
    let mut app = App::new();
    for i in 0..100 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            text: format!("line {i}"),
            kind: LineKind::Normal,
            detail: None,
            tool_state: None,
        });
    }
    app.scroll_up();
    assert!(!app.viewport.follow_bottom);
    assert_eq!(app.viewport.scroll_offset, app.viewport.page_step());
}

#[test]
fn scroll_down_reenters_follow_bottom() {
    let mut app = App::new();
    for i in 0..100 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            text: format!("line {i}"),
            kind: LineKind::Normal,
            detail: None,
            tool_state: None,
        });
    }
    let step = app.viewport.page_step();
    app.scroll_up();
    app.scroll_up();
    assert_eq!(app.viewport.scroll_offset, 2 * step);
    app.scroll_down();
    assert_eq!(app.viewport.scroll_offset, step);
    app.viewport.scroll_offset = step;
    app.scroll_down();
    assert!(app.viewport.follow_bottom);
    assert_eq!(app.viewport.scroll_offset, 0);
}

#[test]
fn wheel_step_is_small_so_a_swipe_composes_smoothly() {
    let mut app = App::new();
    for i in 0..100 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            text: format!("line {i}"),
            kind: LineKind::Normal,
            detail: None,
            tool_state: None,
        });
    }
    // A few wheel ticks walk a few lines each — not half a screen per tick.
    app.scroll_wheel_up();
    app.scroll_wheel_up();
    assert!(!app.viewport.follow_bottom);
    assert_eq!(app.viewport.scroll_offset, 2 * WHEEL_STEP);
    // Wheeling back down re-enters follow-bottom exactly at the tail.
    for _ in 0..=WHEEL_STEP {
        app.scroll_wheel_down();
    }
    assert!(app.viewport.follow_bottom);
    assert_eq!(app.viewport.scroll_offset, 0);
}

#[test]
fn keyboard_page_step_stays_half_screen_despite_wheel_step() {
    let mut app = App::new();
    for i in 0..100 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            text: format!("line {i}"),
            kind: LineKind::Normal,
            detail: None,
            tool_state: None,
        });
    }
    app.viewport.set_visible(1000, 40); // a real render: 40-row pane
    app.scroll_up();
    assert_eq!(app.viewport.scroll_offset, app.viewport.page_step());
    assert!(app.viewport.page_step() > WHEEL_STEP);
}

#[test]
fn enter_sinks_to_bottom_even_without_a_message() {
    let mut app = App::new();
    // The user is reading history when they press Enter.
    app.viewport.follow_bottom = false;
    app.viewport.scroll_offset = 40;
    // Empty Enter: no action, but the view snaps to the newest lines — the
    // key is the "back to now" gesture whether or not anything is sent.
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(
        app.viewport.follow_bottom,
        "Enter must sink the transcript even with an empty composer"
    );
    assert_eq!(app.viewport.scroll_offset, 0);
    // The steer path (running turn) sinks the same way.
    app.viewport.follow_bottom = false;
    app.viewport.scroll_offset = 40;
    app.running = true;
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(app.viewport.follow_bottom);
    // And a real submit sinks at the key press, not only at the send site.
    app.running = false;
    app.viewport.follow_bottom = false;
    app.viewport.scroll_offset = 40;
    app.composer.insert_str("hello");
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Some(Action::Submit("hello".to_string()))
    );
    assert!(app.viewport.follow_bottom);
}

#[test]
fn ctrl_j_inserts_newline_not_submit() {
    // The terminal-wide newline chord: a raw-mode legacy terminal reports
    // 0x0A for both Enter chords and crossterm maps it to Ctrl+J.
    let mut app = App::new();
    app.composer.insert_str("a");
    assert_eq!(
        app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL),
        None
    );
    assert_eq!(app.composer.text(), "a\n");
}

#[test]
fn ctrl_end_jumps_to_the_newest_lines() {
    let mut app = App::new();
    app.viewport.follow_bottom = false;
    app.viewport.scroll_offset = 40;
    assert_eq!(app.handle_key(KeyCode::End, KeyModifiers::CONTROL), None);
    assert!(app.viewport.follow_bottom);
    assert_eq!(app.viewport.scroll_offset, 0);
    // Plain End keeps its composer meaning (cursor to line end) and never
    // touches the viewport — the scroll hint names Ctrl+End for a reason.
    app.viewport.follow_bottom = false;
    app.viewport.scroll_offset = 40;
    app.composer.insert_str("ab");
    assert_eq!(app.handle_key(KeyCode::End, KeyModifiers::NONE), None);
    assert!(!app.viewport.follow_bottom);
    assert_eq!(app.viewport.scroll_offset, 40);
}
