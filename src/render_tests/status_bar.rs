//! The bottom status bar.

use super::super::*;
use crate::app::TuiEvent;

// ── Bottom status bar (mode block left, status-driven right slot) ──────────

#[test]
fn status_bar_shows_mode_with_switch_hint() {
    let mut app = App::new();
    app.push_system("hello");
    let snap = snapshot_text(&mut app, 80, 24);
    let bar = snap.lines().last().unwrap_or_default();
    // The badge teaches its toggle where it lives (Claude Code style).
    assert_eq!(
        bar, "auto mode on (shift+tab to cycle)",
        "mode block alone on the bottom bar, no key-hint string:\n{snap}"
    );
}

#[test]
fn process_row_sits_below_a_gap_row() {
    let mut app = App::new();
    app.push_system("hello");
    let snap = snapshot_text(&mut app, 80, 24);
    let lines: Vec<&str> = snap.lines().collect();
    let pos = lines
        .iter()
        .position(|l| l.contains("Idle"))
        .expect("process row present");
    assert!(
        pos > 0 && lines[pos - 1].trim().is_empty(),
        "transcript content must not crowd the process row:\n{snap}"
    );
}

#[test]
fn status_bar_right_slot_prefers_upgrade_hint() {
    let mut app = App::new();
    app.push_system("hello");
    app.upgrade_hint = Some("⬆ phimint 0.9.9 available - run: brew upgrade".to_string());
    let snap = snapshot_text(&mut app, 80, 24);
    let bar = snap.lines().last().unwrap_or_default();
    assert!(
        bar.starts_with("auto mode on (shift+tab to cycle)") && bar.ends_with("brew upgrade"),
        "upgrade hint must sit in the right slot:\n{snap}"
    );
}

#[test]
fn upgrade_notice_is_committed_to_transcript() {
    // Session 20261018_7844215b: the hint lived only in the status slot, which
    // dies with the alt-screen — a quick start/exit showed the user nothing.
    // The committed transcript is what the exit replay reprints. The check
    // itself is the product's (ShellSpec update slot); its hint arrives as a
    // plain status hint.
    let mut app = App::new();
    app.handle_event(TuiEvent::StatusHint(
        "⬆ phimint 0.9.9 available - run: brew upgrade".to_string(),
    ));
    assert!(app.upgrade_hint.is_some(), "status-slot hint must remain");
    let snap = snapshot_text(&mut app, 80, 24);
    assert!(
        snap.contains("0.9.9 available"),
        "notice must be in the committed transcript (exit replay source):\n{snap}"
    );
}

#[test]
fn status_bar_right_slot_shows_scroll_position_off_bottom() {
    let mut app = App::new();
    for i in 0..100 {
        app.push_system(&format!("line {i}"));
    }
    // First frame records the pane geometry the scroll math runs on; at the
    // bottom there is nothing to report.
    let snap = snapshot_text(&mut app, 80, 20);
    let bar = snap.lines().last().unwrap_or_default();
    assert_eq!(
        bar, "auto mode on (shift+tab to cycle)",
        "quiet right slot while following the bottom:\n{snap}"
    );

    app.scroll_up();
    let snap = snapshot_text(&mut app, 80, 20);
    let bar = snap.lines().last().unwrap_or_default();
    assert!(
        bar.contains("scroll ") && bar.contains("(Ctrl+End to bottom)"),
        "scrolled off the bottom must show the scroll position:\n{snap}"
    );

    app.scroll_to_bottom();
    let snap = snapshot_text(&mut app, 80, 20);
    let bar = snap.lines().last().unwrap_or_default();
    assert_eq!(
        bar, "auto mode on (shift+tab to cycle)",
        "back at the bottom the slot goes quiet again:\n{snap}"
    );
}

#[test]
fn status_bar_elides_long_right_notices() {
    let mut app = App::new();
    app.push_system("hello");
    app.upgrade_hint = Some(format!(
        "⬆ phimint 9.9.9 available - run: {}",
        "x".repeat(200)
    ));
    let snap = snapshot_text(&mut app, 40, 24);
    let bar = snap.lines().last().unwrap_or_default();
    assert!(
        bar.starts_with("auto mode on (shift+tab to cycle)"),
        "mode block keeps its columns:\n{snap}"
    );
    assert!(
        bar.contains("..."),
        "an overlong right notice elides instead of wrapping:\n{snap}"
    );
    assert!(
        !bar.contains("xxxxxxxxxx"),
        "the elision must cut the overflow away:\n{snap}"
    );
}
