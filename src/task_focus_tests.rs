//! Design-verify tests for the three-state task-frame machine (20261103,
//! rows reworked Rev 2026-11-04).
//!
//! Covers the S0/S1/S2 transition table, selection by row id, the D10
//! landing rule, ack-on-open, typing takeover (打字即接管) and the two death
//! sweeps (all-complete / turn-end) with their exemptions.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::{App, FocusTarget, SubAgentState, SubAgentStatus};
use crate::task_layout::status_glyph;

// ── fixtures ────────────────────────────────────────────────────────────────

/// A running sub-agent row whose spawn time is `offset` seconds ago, so row
/// order (spawn order) is deterministic in tests.
fn running_agent(name: &str, offset: u64) -> SubAgentState {
    SubAgentState {
        name: name.to_string(),
        status: SubAgentStatus::Running,
        files: Vec::new(),
        started_at: Instant::now() - Duration::from_secs(offset),
        completed_at: None,
        last_tool_at: Instant::now(),
        events: Vec::new(),
    }
}

fn insert(app: &mut App, id: &str, name: &str, offset: u64) {
    app.sub_agents
        .insert(id.to_string(), running_agent(name, offset));
}

fn ctrl_t(app: &mut App) {
    app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL);
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(code, KeyModifiers::NONE);
}

fn sel(app: &App) -> Option<String> {
    app.task_panel.selected_id().map(str::to_string)
}

fn ids(app: &App) -> Vec<String> {
    app.task_rows().iter().map(|r| r.id().to_string()).collect()
}

fn fail(app: &mut App, id: &str) {
    app.mark_sub_agent_finished(
        id,
        SubAgentStatus::Failed {
            summary: "boom".into(),
        },
    );
}

// ── S0/S1/S2 transition table (design detail 2) ─────────────────────────────

#[test]
fn ctrl_t_toggles_s0_s2_and_lands_on_the_last_row() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 2);
    insert(&mut app, "root/b", "b", 1);

    assert!(!app.task_panel.frame_open, "S0 initially");
    ctrl_t(&mut app);
    assert!(app.task_panel.frame_open, "Ctrl+T opens the frame");
    assert_eq!(
        sel(&app).as_deref(),
        Some("root/b"),
        "S2 lands on the last row (no failures)"
    );

    ctrl_t(&mut app);
    assert!(!app.task_panel.frame_open, "Ctrl+T collapses");
    assert_eq!(app.task_panel.focus, FocusTarget::Input);
}

#[test]
fn ctrl_t_on_an_empty_set_is_a_no_op() {
    let mut app = App::new();
    // The frame follows the work 框跟着事走: no rows, no face — the key must not open an empty frame.
    ctrl_t(&mut app);
    assert!(!app.task_panel.frame_open);
    assert_eq!(app.task_panel.focus, FocusTarget::Input);
    assert!(!app.should_show_task_panel());
}

#[test]
fn up_opens_s2_and_down_past_the_full_list_drops_s1() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 1);

    // ↑ at the composer (S0) opens the frame and enters the list.
    key(&mut app, KeyCode::Up);
    assert!(app.task_panel.frame_open);
    assert_eq!(sel(&app).as_deref(), Some("root/a"));

    // ↓ past the FULL-list last row (not the window's) → S1.
    key(&mut app, KeyCode::Down);
    assert!(app.task_panel.frame_open, "frame stays open in S1");
    assert_eq!(app.task_panel.focus, FocusTarget::Input);

    // ↑ re-enters S2 and re-lands per D10 (no failures → the last row).
    key(&mut app, KeyCode::Up);
    assert_eq!(sel(&app).as_deref(), Some("root/a"));
}

#[test]
fn escape_drops_s2_to_s1_and_the_second_escape_falls_through() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 1);
    ctrl_t(&mut app);
    assert_eq!(sel(&app).as_deref(), Some("root/a"));

    // D8: the first Esc only deselects — the draft must survive untouched.
    assert!(app.task_list_escape(), "S2 consumes Esc");
    assert!(app.task_panel.frame_open);
    assert_eq!(app.task_panel.focus, FocusTarget::Input);

    // In S1 the key is not ours: normal Esc semantics take it.
    assert!(!app.task_list_escape(), "S1 must not consume Esc");
}

#[test]
fn home_end_and_page_move_the_selection() {
    let mut app = App::new();
    for i in 0..10 {
        insert(
            &mut app,
            &format!("root/{i:02}"),
            &format!("n{i:02}"),
            20 - i,
        );
    }
    ctrl_t(&mut app);
    assert_eq!(sel(&app).as_deref(), Some("root/09"));

    key(&mut app, KeyCode::Home);
    assert_eq!(sel(&app).as_deref(), Some("root/00"));
    key(&mut app, KeyCode::End);
    assert_eq!(sel(&app).as_deref(), Some("root/09"));

    // PgUp/PgDn move by one window page (view_rows, default 6).
    key(&mut app, KeyCode::PageUp);
    assert_eq!(sel(&app).as_deref(), Some("root/03"));
    key(&mut app, KeyCode::PageDown);
    assert_eq!(sel(&app).as_deref(), Some("root/09"));
}

// ── D10 landing + ack-on-open ───────────────────────────────────────────────

#[test]
fn d10_landing_prefers_the_first_unacked_failure() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 3);
    insert(&mut app, "root/b", "b", 2);
    insert(&mut app, "root/c", "c", 1);
    fail(&mut app, "root/b");

    key(&mut app, KeyCode::Up); // S0 → open + enter one step from the lesion
    assert_eq!(sel(&app).as_deref(), Some("root/b"));
    assert!(
        app.task_panel.unacked_failures.is_empty(),
        "opening acks every outstanding failure (D10)"
    );
}

#[test]
fn opening_clears_the_new_badge() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 1);
    fail(&mut app, "root/a");
    assert_eq!(app.task_panel.unacked_failures.len(), 1);

    ctrl_t(&mut app);
    assert!(app.task_panel.unacked_failures.is_empty());
    let snap = crate::render::snapshot_text(&mut app, 60, 12);
    assert!(!snap.contains("1 new"), "badge clears on open: {snap}");
    assert!(snap.contains("Tasks (1)"), "got:\n{snap}");
}

// ── typing takeover (Rev 2026-11-04) ────────────────────────────────────────

#[test]
fn typing_hands_the_keyboard_back_to_the_composer() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 1);
    ctrl_t(&mut app); // S2: pinned on root/a
    assert!(sel(&app).is_some());

    app.handle_key(KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(app.task_panel.focus, FocusTarget::Input, "typing takeover");
    assert_eq!(app.composer.text(), "h", "the key still types");

    // The walked-away pin demotes to a corpse (pin-release — reversible against
    // the design table's "the row does not change" reading: the root reply must be
    // readable and a lingering pin would block the all-complete sweep).
    assert!(app.task_panel.touched.contains("root/a"));

    // Enter steals too: re-pin, then submit (empty) — focus ends at Input.
    key(&mut app, KeyCode::Up);
    assert!(sel(&app).is_some());
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.task_panel.focus, FocusTarget::Input);
}

// ── display set + death sweeps (Rev 2026-11-04) ────────────────────────────

#[test]
fn a_touchedless_terminal_row_hides_the_moment_it_lands() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 2);
    insert(&mut app, "root/b", "b", 1);
    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);

    // Hide-on-complete 完成即隐: the row leaves the display set at the terminal …
    assert_eq!(ids(&app), ["root/b"]);
    // … but the store keeps it until a death sweep (late fan-in reports).
    assert!(app.sub_agents.contains_key("root/a"));
    app.turn_end_sweep();
    assert!(
        !app.sub_agents.contains_key("root/a"),
        "untouched terminal rows go at the sweep"
    );
}

#[test]
fn auto_sweep_clears_the_face_when_everything_is_done() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 1);
    ctrl_t(&mut app); // pin the row — a selection waits for the next death point
    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);
    assert!(
        app.sub_agents.contains_key("root/a"),
        "the pinned row blocks the sweep even at all-complete"
    );
    assert_eq!(
        ids(&app),
        ["root/a"],
        "inspection freeze: the pinned row shows"
    );

    // Walk away: the pin demotes to a corpse and the sweep is now due.
    assert!(app.task_list_escape());
    assert!(!app.sub_agents.contains_key("root/a"), "all-complete sweep");
    assert!(
        !app.task_panel.frame_open,
        "the frame follows the work: no rows, no face"
    );
    assert_eq!(app.task_panel.focus, FocusTarget::Input);
}

#[test]
fn turn_end_sweep_removes_corpses_but_spares_the_pin_and_the_remnant() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 3);
    insert(&mut app, "root/b", "b", 2);
    insert(&mut app, "root/c", "c", 1);
    ctrl_t(&mut app); // pins root/c (last row)
    key(&mut app, KeyCode::Up); // root/b
    key(&mut app, KeyCode::Up); // root/a — every row touched
    key(&mut app, KeyCode::End); // back to root/c

    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);
    fail(&mut app, "root/b");
    app.mark_sub_agent_finished("root/c", SubAgentStatus::Done);

    assert_eq!(ids(&app), ["root/a", "root/b", "root/c"]);
    app.turn_end_sweep();
    // root/a was a swept corpse; the pin (root/c) and the unacked failure
    // (root/b) survive — principle ① / the worst residue.
    assert_eq!(ids(&app), ["root/b", "root/c"]);
    assert!(!app.sub_agents.contains_key("root/a"));
    assert!(app.sub_agents.contains_key("root/b"));
    assert!(app.sub_agents.contains_key("root/c"));
    assert!(app.task_panel.frame_open, "the face still has rows");
}

#[test]
fn an_unacked_failure_remnant_lingers_until_seen() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 1);
    fail(&mut app, "root/a");

    // No auto-expand (F5, retired): the remnant row on the live face is the
    // alarm — and it outlives every sweep until the user comes to look.
    assert!(!app.task_panel.frame_open);
    assert_eq!(ids(&app), ["root/a"], "the remnant keeps its row");
    app.turn_end_sweep();
    assert!(
        app.sub_agents.contains_key("root/a"),
        "the worst residue is the red failure row"
    );

    // Coming to look (frame open) lands on it and acks it away.
    ctrl_t(&mut app);
    assert_eq!(sel(&app).as_deref(), Some("root/a"));
    assert!(app.task_panel.unacked_failures.is_empty());
}

// ── three-way encoding (design detail 6) ────────────────────────────────────

#[test]
fn failure_and_cancel_encode_three_ways_each() {
    let mut app = App::new();
    insert(&mut app, "root/bad", "bad", 2);
    insert(&mut app, "root/nope", "nope", 1);
    fail(&mut app, "root/bad");
    app.mark_sub_agent_finished("root/nope", SubAgentStatus::Cancelled);

    // Failure: red `!` row + a `FAILED (…)` verdict and ledger marker.
    let (glyph, tint) = status_glyph(&SubAgentStatus::Failed {
        summary: "boom".into(),
    });
    assert_eq!(glyph, "!");
    assert_eq!(tint, crate::task_layout::RowTint::Error);
    assert!(
        app.transcript
            .output
            .iter()
            .any(|l| l.text == "* [root/bad] FAILED (boom)"),
        "transcript must record the failure: {:?}",
        app.transcript
            .output
            .iter()
            .map(|l| &l.text)
            .collect::<Vec<_>>()
    );

    // Cancel: yellow `-` row + a `CANCELLED` marker — never red (D9).
    let (glyph, tint) = status_glyph(&SubAgentStatus::Cancelled);
    assert_eq!(glyph, "-");
    assert_eq!(tint, crate::task_layout::RowTint::Warn);
    assert!(
        app.transcript
            .output
            .iter()
            .any(|l| l.text == "* [root/nope] CANCELLED"),
        "transcript must record the cancel"
    );
}
