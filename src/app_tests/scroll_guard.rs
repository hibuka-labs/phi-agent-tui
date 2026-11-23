//! Scroll-companion arrow guard wiring (FinalShell dual-stream): the guard's
//! decision must reach the key dispatch entry, so a wheel+arrow batch never
//! flips input history — while the off switch and Alt+↑/↓ keep their behavior.

use super::super::*;
use crossterm::event::{Event, KeyEvent, MouseEvent, MouseEventKind};

fn wheel() -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// protects: a bare ↑ riding in with a wheel event never recalls history —
/// the synthesized companion is dropped at the dispatch entry.
/// fails_when: the composer flips to a history entry while scrolling.
/// no_claim: the wheel half (the viewport scroll is the wanted behavior).
#[test]
fn a_companion_arrow_never_recalls_history() {
    let mut app = App::new();
    app.set_scroll_arrow_guard(true);
    app.record_input("saved entry");
    app.composer.insert_str("draft");
    app.note_input_batch(&[wheel(), key(KeyCode::Up)]);
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(
        app.composer.text(),
        "draft",
        "the companion arrow must not flip history"
    );
}

/// protects: order-independence at the wiring level — a batch that leads
/// with the arrow is covered too (the pre-scan runs before dispatch).
/// fails_when: only wheel-first batches are suppressed. no_claim: the
/// guard's own ordering rules (pinned in its unit tests).
#[test]
fn the_arrow_before_the_wheel_in_one_batch_is_dropped_too() {
    let mut app = App::new();
    app.set_scroll_arrow_guard(true);
    app.record_input("saved entry");
    app.composer.insert_str("draft");
    app.note_input_batch(&[key(KeyCode::Up), wheel()]);
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "draft");
}

/// protects: the opt-out is a full no-op — a product that never armed the
/// guard keeps every existing key behavior, dual stream or not.
/// fails_when: suppression runs regardless of the switch. no_claim: what
/// an opted-in product does with the same batch.
#[test]
fn the_guard_is_off_unless_the_product_opts_in() {
    let mut app = App::new();
    app.record_input("saved entry");
    app.composer.insert_str("draft");
    app.note_input_batch(&[wheel(), key(KeyCode::Up)]);
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(
        app.composer.text(),
        "saved entry",
        "recall still works without the opt-in"
    );
}

/// protects: Alt+↑/↓ keep cursor movement inside the window — the
/// synthesized stream is always bare, so chords are never dropped.
/// fails_when: the guard sweeps modifier arrows up with the bare ones.
/// no_claim: where the cursor lands (the composer's business).
#[test]
fn alt_arrow_keeps_cursor_movement_inside_the_window() {
    let mut app = App::new();
    app.set_scroll_arrow_guard(true);
    app.record_input("saved entry");
    app.composer.insert_str("a\nb\nc");
    app.note_input_batch(&[wheel()]);
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::ALT), None);
    assert_eq!(app.composer.cursor().0, 1, "Alt+↑ still moves the cursor");
    assert_eq!(app.composer.text(), "a\nb\nc", "and never recalls");
}
