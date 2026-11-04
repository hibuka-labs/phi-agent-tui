//! Design-verify tests for the three-state task-frame machine (20261103).
//!
//! Covers the S0/S1/S2 transition table, the D10 landing rule, the failure
//! auto-expand (F5/F9), the ledger counts' 对账 rule, and the reap TTLs.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::{App, FocusTarget, SubAgentState, SubAgentStatus};
use crate::task_layout::{LedgerMark, parse_ledger_marker, status_glyph};

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

fn row_index(app: &App, id: &str) -> usize {
    app.task_rows()
        .iter()
        .position(|r| matches!(r, crate::task_panel::TaskRow::SubAgent(p) if p == id))
        .expect("row present")
}

fn row_visible(app: &App, idx: usize) -> bool {
    let off = app.task_panel.window_offset;
    idx >= off && idx < off + app.task_panel.view_rows
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
        app.task_panel.focus,
        FocusTarget::TaskList(1),
        "S2 lands on the last row (no failures)"
    );

    ctrl_t(&mut app);
    assert!(!app.task_panel.frame_open, "Ctrl+T collapses");
    assert_eq!(app.task_panel.focus, FocusTarget::Input);
}

#[test]
fn ctrl_t_on_an_empty_list_lands_s1_and_the_frame_says_so() {
    let mut app = App::new();
    // S0 空: no task history — the key is live and the frame explains itself.
    ctrl_t(&mut app);
    assert!(app.task_panel.frame_open);
    assert_eq!(app.task_panel.focus, FocusTarget::Input, "S1 has no row");
    let snap = crate::render::snapshot_text(&mut app, 60, 12);
    assert!(snap.contains("Tasks (0)"), "got:\n{snap}");
    assert!(snap.contains("No sub-tasks yet"), "got:\n{snap}");
}

#[test]
fn up_opens_s2_and_down_past_the_full_list_drops_s1() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 1);

    // ↑ at the composer (S0) opens the frame and enters the list.
    key(&mut app, KeyCode::Up);
    assert!(app.task_panel.frame_open);
    assert_eq!(app.task_panel.focus, FocusTarget::TaskList(0));

    // ↓ past the FULL-list last row (not the window's) → S1.
    key(&mut app, KeyCode::Down);
    assert!(app.task_panel.frame_open, "frame stays open in S1");
    assert_eq!(app.task_panel.focus, FocusTarget::Input);

    // ↑ re-enters S2 and re-lands per D10 (no failures → the last row).
    key(&mut app, KeyCode::Up);
    assert_eq!(app.task_panel.focus, FocusTarget::TaskList(0));
}

#[test]
fn escape_drops_s2_to_s1_and_the_second_escape_falls_through() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 1);
    ctrl_t(&mut app);
    assert_eq!(app.task_panel.focus, FocusTarget::TaskList(0));

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
    assert_eq!(app.task_panel.focus, FocusTarget::TaskList(9));

    key(&mut app, KeyCode::Home);
    assert_eq!(app.task_panel.focus, FocusTarget::TaskList(0));
    key(&mut app, KeyCode::End);
    assert_eq!(app.task_panel.focus, FocusTarget::TaskList(9));

    // PgUp/PgDn move by one window page (view_rows, default 6).
    key(&mut app, KeyCode::PageUp);
    assert_eq!(app.task_panel.focus, FocusTarget::TaskList(3));
    key(&mut app, KeyCode::PageDown);
    assert_eq!(app.task_panel.focus, FocusTarget::TaskList(9));
}

// ── failure auto-expand (design details 3/5, F5/F9/D10) ─────────────────────

#[test]
fn auto_expand_lands_s1_anchoring_the_failed_row_in_the_window() {
    let mut app = App::new();
    for i in 0..20 {
        insert(
            &mut app,
            &format!("root/{i:02}"),
            &format!("n{i:02}"),
            30 - i,
        );
    }
    let failed_idx = row_index(&app, "root/15");

    // First failure while collapsed pops the frame WITHOUT stealing focus.
    app.mark_sub_agent_finished(
        "root/15",
        SubAgentStatus::Failed {
            summary: "boom".into(),
        },
    );
    assert!(app.task_panel.frame_open, "auto-expand fired");
    assert_eq!(
        app.task_panel.focus,
        FocusTarget::Input,
        "auto-expand lands S1 — the user is typing"
    );
    assert_eq!(app.task_panel.unacked_failures, 1);
    assert!(
        row_visible(&app, failed_idx),
        "F5: the failed row must be inside the 6-row window (off={} rows={})",
        app.task_panel.window_offset,
        app.task_panel.view_rows
    );
}

#[test]
fn s2_entry_on_failure_lands_the_failed_row_and_clears_the_badge() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 2);
    insert(&mut app, "root/b", "b", 1);
    app.mark_sub_agent_finished(
        "root/a",
        SubAgentStatus::Failed {
            summary: "boom".into(),
        },
    );
    assert_eq!(app.task_panel.unacked_failures, 1);

    // D10: ↑ from S1 enters the list one step from the lesion.
    key(&mut app, KeyCode::Up);
    assert_eq!(
        app.task_panel.focus,
        FocusTarget::TaskList(row_index(&app, "root/a"))
    );
    assert_eq!(
        app.task_panel.unacked_failures, 0,
        "entering S2 acks the failures"
    );

    // And the title badge is gone with it.
    app.task_panel.frame_open = true;
    let snap = crate::render::snapshot_text(&mut app, 60, 12);
    assert!(
        !snap.contains("1 new"),
        "badge must clear on S2 entry: {snap}"
    );
}

#[test]
fn manual_collapse_outranks_auto_expand_for_the_rest_of_the_turn() {
    let mut app = App::new();
    insert(&mut app, "root/a", "a", 1);

    // Open then manually collapse: F9 — no bounce-back this turn.
    ctrl_t(&mut app);
    ctrl_t(&mut app);
    assert!(!app.task_panel.frame_open);
    app.mark_sub_agent_finished(
        "root/a",
        SubAgentStatus::Failed {
            summary: "boom".into(),
        },
    );
    assert!(
        !app.task_panel.frame_open,
        "manual collapse outranks the auto-expand"
    );

    // The next turn resets the budget.
    app.begin_user_turn("next");
    app.mark_sub_agent_finished(
        "root/b",
        SubAgentStatus::Failed {
            summary: "boom".into(),
        },
    );
    // (root/b has no row — finalize is a no-op — so arm via a real row.)
    insert(&mut app, "root/c", "c", 1);
    app.mark_sub_agent_finished(
        "root/c",
        SubAgentStatus::Failed {
            summary: "boom".into(),
        },
    );
    assert!(app.task_panel.frame_open, "new turn may auto-expand again");
}

// ── counts ledger (design detail 4, D9) ─────────────────────────────────────

#[test]
fn counts_reconcile_with_the_ledger_markers() {
    let mut app = App::new();
    insert(&mut app, "root/ok", "ok", 3);
    insert(&mut app, "root/bad", "bad", 2);
    insert(&mut app, "root/nope", "nope", 1);
    app.mark_sub_agent_finished("root/ok", SubAgentStatus::Done);
    app.mark_sub_agent_finished(
        "root/bad",
        SubAgentStatus::Failed {
            summary: "boom".into(),
        },
    );
    app.mark_sub_agent_finished("root/nope", SubAgentStatus::Cancelled);

    let counts = &app.task_panel.counts;
    assert_eq!(counts.done, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.cancelled, 1, "cancel is its own yellow bucket (D9)");

    // 对账: ✓ + ✗ + — = terminal marker count (started excluded), and each
    // marker agrees with its bucket.
    let marks: Vec<LedgerMark> = app
        .transcript
        .output
        .iter()
        .filter_map(|l| parse_ledger_marker(&l.text))
        .collect();
    assert_eq!(marks.len(), counts.total());
    assert_eq!(
        marks.iter().filter(|m| **m == LedgerMark::Done).count(),
        counts.done
    );
    assert_eq!(
        marks.iter().filter(|m| **m == LedgerMark::Failed).count(),
        counts.failed
    );
    assert_eq!(
        marks
            .iter()
            .filter(|m| **m == LedgerMark::Cancelled)
            .count(),
        counts.cancelled
    );
}

#[test]
fn failure_and_cancel_encode_three_ways_each() {
    let mut app = App::new();
    insert(&mut app, "root/bad", "bad", 2);
    insert(&mut app, "root/nope", "nope", 1);
    app.mark_sub_agent_finished(
        "root/bad",
        SubAgentStatus::Failed {
            summary: "boom".into(),
        },
    );
    app.mark_sub_agent_finished("root/nope", SubAgentStatus::Cancelled);

    // Failure: red `!` row + `FAILED (…)` marker + the ✗ bucket.
    let (glyph, tint) = status_glyph(&SubAgentStatus::Failed {
        summary: "boom".into(),
    });
    assert_eq!(glyph, "!");
    assert_eq!(tint, crate::task_layout::RowTint::Error);
    assert!(
        app.transcript
            .output
            .iter()
            .any(|l| l.text.contains("FAILED (boom)")),
        "transcript must record the failure: {:?}",
        app.transcript
            .output
            .iter()
            .map(|l| &l.text)
            .collect::<Vec<_>>()
    );

    // Cancel: yellow `-` row + `CANCELLED` marker + the — bucket, never ✗.
    let (glyph, tint) = status_glyph(&SubAgentStatus::Cancelled);
    assert_eq!(glyph, "-");
    assert_eq!(tint, crate::task_layout::RowTint::Warn);
    assert!(
        app.transcript
            .output
            .iter()
            .any(|l| l.text.contains("CANCELLED")),
        "transcript must record the cancel"
    );
    assert_eq!(app.task_panel.counts.failed, 1);
    assert_eq!(app.task_panel.counts.cancelled, 1);
}

#[test]
fn failed_rows_get_the_30s_review_window() {
    let mut app = App::new();
    let mut failed = running_agent("bad", 50);
    failed.status = SubAgentStatus::Failed {
        summary: "boom".into(),
    };
    failed.completed_at = Some(Instant::now() - Duration::from_secs(10));
    app.sub_agents.insert("root/bad".to_string(), failed);

    // 10s old: inside the 30s F8 window even though Done would be gone at 3s.
    assert!(!app.cleanup_completed_agents());
    assert!(app.sub_agents.contains_key("root/bad"));

    app.sub_agents.get_mut("root/bad").unwrap().completed_at =
        Some(Instant::now() - Duration::from_secs(35));
    assert!(app.cleanup_completed_agents());
    assert!(!app.sub_agents.contains_key("root/bad"));
}

// ── resume recount (design detail 4) ────────────────────────────────────────

#[test]
fn resume_recount_rebuilds_the_counts_from_transcript_markers() {
    let mut app = App::new();
    // A restarted session has no rows but the permanent markers replay.
    app.transcript.push(phi_tui::lines::OutputLine {
        text: "* [root/a] started".to_string(),
        kind: phi_tui::lines::LineKind::Tool,
        spans: None,
        original: None,
        detail: None,
        tool_state: None,
    });
    app.transcript.push(phi_tui::lines::OutputLine {
        text: "* [root/a] done (2m13s)".to_string(),
        kind: phi_tui::lines::LineKind::Tool,
        spans: None,
        original: None,
        detail: None,
        tool_state: None,
    });
    app.transcript.push(phi_tui::lines::OutputLine {
        text: "* [root/b] FAILED (exit 1)".to_string(),
        kind: phi_tui::lines::LineKind::Tool,
        spans: None,
        original: None,
        detail: None,
        tool_state: None,
    });
    app.transcript.push(phi_tui::lines::OutputLine {
        text: "* [root/c] CANCELLED".to_string(),
        kind: phi_tui::lines::LineKind::Tool,
        spans: None,
        original: None,
        detail: None,
        tool_state: None,
    });

    app.recount_task_ledger();
    let counts = &app.task_panel.counts;
    assert_eq!((counts.done, counts.failed, counts.cancelled), (1, 1, 1));
    assert_eq!(counts.total(), 3, "started is not a terminal marker");
}
