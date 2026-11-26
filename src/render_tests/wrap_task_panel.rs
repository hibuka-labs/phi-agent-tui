//! Wrapping contracts and the process row.

use super::super::*;
use crate::app::TuiEvent;
use crate::theme::{BrandRow, ShellToken};
use phi_agent::{RuntimeEvent, SessionId};

// ── Task panel column accounting ─────────────────────────────────────────────
// The name / files / activity cells must be laid out in terminal columns. They
// used to mix three accountings — `chars().count()`, `format!("{:<w$}")` (chars)
// and `result.len()` (bytes) — and the bytes one slices mid-char. The column
// accounting itself now lives in `task_layout` (elide/pad per display column,
// CJK width tests in `task_layout_tests`); the render-level rows are covered by
// the task-panel snapshot tests below.

#[test]
fn prose_longer_than_width_must_wrap_not_clip() {
    let mut app = App::new();
    // Assistant prose that clearly overflows 80 columns.
    let long = "The policy lives in the wake module and batches completions behind a quiet window so a burst of finished tasks costs one notification turn, not ten of them.";
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::TextDelta {
        session_id: SessionId::new(1),
        text: long.to_string(),
        agent_id: None,
        trace_id: None,
    }));
    app.handle_event(TuiEvent::TurnDone);
    let snap = snapshot_text(&mut app, 80, 24);
    // Wrapped, not clipped: the tail is on screen and every word survives.
    let has_head = snap.lines().any(|l| l.contains("The policy lives in"));
    let has_tail = snap.lines().any(|l| l.contains("them."));
    assert!(has_head, "head clipped:\n{snap}");
    assert!(has_tail, "TAIL LOST (clipped at width):\n{snap}");
    for w in long.split_whitespace() {
        assert!(snap.contains(w), "word {w:?} lost (clipped):\n{snap}");
    }
    // Real wrap: the sentence spans several visual rows, not one hard-cut row.
    let prose_rows = snap
        .lines()
        .filter(|l| l.contains("policy") || l.contains("window so") || l.contains("them."))
        .count();
    assert!(
        prose_rows >= 2,
        "prose must wrap into multiple rows:\n{snap}"
    );
}

/// The same clip bug on the uncommitted streaming tail: prose mid-stream must
/// wrap, not vanish at the pane edge until it commits.
#[test]
fn streaming_tail_longer_than_width_must_wrap_not_clip() {
    let mut app = App::new();
    let long = "Streaming answers stay uncommitted until the turn ends, so the live tail is the only place a reader sees the sentence, and its ending must remain on screen the whole time.";
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::TextDelta {
        session_id: SessionId::new(1),
        text: long.to_string(),
        agent_id: None,
        trace_id: None,
    }));
    // No TurnDone: the text is still the streaming tail.
    let snap = snapshot_text(&mut app, 80, 24);
    let has_head = snap
        .lines()
        .any(|l| l.contains("Streaming answers stay uncommitted"));
    let has_tail = snap.lines().any(|l| l.contains("whole time."));
    assert!(has_head, "head clipped:\n{snap}");
    assert!(has_tail, "TAIL LOST (clipped at width):\n{snap}");
    for w in long.split_whitespace() {
        assert!(snap.contains(w), "word {w:?} lost (clipped):\n{snap}");
    }
}

/// The opposite contract on `push_styled_line` rows: they must NOT soft-wrap.
/// phi-tui documents "**no** soft-wrap (the wordmark must stay whole; ratatui
/// clips on narrow terminals), no re-wrap on width change (`original: None`)".
/// Brand art and a full-width `─` rule are fixed-width chrome — wrapping the
/// rule (no break points) hard-splits it into dash fragments and a wide glyph
/// row splits at its letter gaps. Render synthetic banner rows wider than the
/// pane and assert each stays ONE visual row (clipped), not a stack of
/// fragments.
#[test]
fn styled_banner_rows_stay_whole_and_do_not_soft_wrap() {
    let mut app = App::new();
    // Synthetic brand rows (the product compiles its own; the contract is on
    // `push_styled_line`): one 100-col rule (no break points) and five 58-glyph
    // wordmark rows — both wider than the 40-col pane below.
    let mut rows = vec![BrandRow {
        text: "─".repeat(100),
        spans: vec![],
    }];
    for _ in 0..5 {
        rows.push(BrandRow {
            text: "█".repeat(58),
            spans: vec![],
        });
    }
    app.push_banner(rows);
    let snap = snapshot_text(&mut app, 40, 30);
    let dash_rows: Vec<&str> = snap
        .lines()
        .filter(|l| !l.trim().is_empty() && l.trim().chars().all(|c| c == '─'))
        .collect();
    // Dash rows on screen: the banner rule (1) plus the composer's top and
    // bottom rules (2, Claude Code style chrome). Any wrap fragment of the
    // 100-col banner rule would push the count above 3.
    assert_eq!(
        dash_rows.len(),
        3,
        "rule must stay one clipped visual row, not wrap into fragments:\n{snap}"
    );
    // And the wordmark keeps one visual row per glyph row (5), not 10 halves.
    let wordmark_rows = snap.lines().filter(|l| l.contains('█')).count();
    assert_eq!(
        wordmark_rows, 5,
        "wordmark must stay whole (5 rows), not split at letter gaps:\n{snap}"
    );
}

/// The opposite contract on quiet-chrome rows (boot notices, terminal notes,
/// the turn verdict): they are prose, not art — they must soft-wrap at the
/// pane width and never clip their tail. The scroll notice runs 89 columns
/// and its actionable tail sits at the end; the row keeps `original`, so the
/// renderer wraps it at display width.
#[test]
fn chrome_notice_rows_soft_wrap_and_never_clip() {
    let mut app = App::new();
    let long = "Note: wheel/trackpad scroll needs View > Allow Mouse Reporting - fn+Up/fn+Down page history";
    app.push_chrome_line(long, ShellToken::Faint);
    let snap = snapshot_text(&mut app, 40, 30);
    for needle in ["Note:", "Reporting", "history"] {
        assert!(snap.contains(needle), "{needle} clipped:\n{snap}");
    }
}

#[test]
fn process_row_renders_note_and_tip_labels() {
    let mut app = App::new();
    app.push_system("hello");
    app.set_tips(vec![crate::tips::TipItem::note(
        "phimint is built on phi-agent - docs: https://docs.phiagent.dev/",
    )]);
    let snap = snapshot_text(&mut app, 80, 24);
    assert!(
        snap.contains("Note: phimint is built on phi-agent"),
        "note text missing from the process row:\n{snap}"
    );
    assert!(
        !snap.contains("Tip:"),
        "a promo must never render as Tip:\n{snap}"
    );

    let mut app = App::new();
    app.push_system("hello");
    app.set_tips(vec![crate::tips::TipItem::tip(
        "Press Ctrl+Y to copy the last reply",
    )]);
    let snap = snapshot_text(&mut app, 80, 24);
    assert!(
        snap.contains("Tip: Press Ctrl+Y to copy the last reply"),
        "tip text missing from the process row:\n{snap}"
    );
    // Status and tip share the one fixed row above the composer.
    let row = snap
        .lines()
        .find(|l| l.contains("Tip:"))
        .unwrap_or_else(|| panic!("tip row missing:\n{snap}"));
    assert!(
        row.contains("Idle"),
        "status and tip must share one row:\n{snap}"
    );
}

#[test]
fn process_row_hides_tip_without_items_and_during_fresh_activity() {
    // No pool (tips disabled): the row is status-only.
    let mut app = App::new();
    app.push_system("hello");
    let snap = snapshot_text(&mut app, 80, 24);
    assert!(
        !snap.contains("Tip:") && !snap.contains("Note:"),
        "no pool renders no tip:\n{snap}"
    );

    // Fresh active stretch: hidden until the activity threshold.
    let mut app = App::new();
    app.push_system("hello");
    app.set_tips(vec![crate::tips::TipItem::tip("# fresh")]);
    app.running = true;
    app.status = AgentStatus::Running {
        phase: Phase::ToolCall {
            tool: "build".into(),
        },
    };
    app.begin_activity();
    let snap = snapshot_text(&mut app, 80, 24);
    assert!(
        !snap.contains("Tip:"),
        "fresh activity hides the tip:\n{snap}"
    );

    // A long-running action past the threshold shows it again.
    app.activity_since = Some(std::time::Instant::now() - std::time::Duration::from_secs(10));
    let snap = snapshot_text(&mut app, 80, 24);
    assert!(
        snap.contains("Tip: # fresh"),
        "long activity shows the tip on the process row:\n{snap}"
    );
}

#[test]
fn process_row_truncates_tip_but_keeps_label() {
    let mut app = App::new();
    app.push_system("hello");
    app.set_tips(vec![crate::tips::TipItem::tip("#".repeat(400))]);
    let snap = snapshot_text(&mut app, 80, 24);
    // One fixed row only: the tip truncates to the row (elide keeps the
    // head, so the `Tip:` label survives) instead of spilling to a band.
    let rows: Vec<&str> = snap.lines().filter(|l| l.contains('#')).collect();
    assert_eq!(rows.len(), 1, "tip must not spill past one row:\n{snap}");
    assert!(
        rows[0].contains("Tip:") && rows[0].contains("..."),
        "label survives truncation (elide keeps the head):\n{snap}"
    );
}

#[test]
fn process_row_puts_tip_one_tab_after_status() {
    let mut app = App::new();
    app.push_system("hello");
    app.set_tips(vec![crate::tips::TipItem::tip("# after")]);
    let snap = snapshot_text(&mut app, 80, 24);
    // The tip rides right behind the status with a tab-wide gap (4 columns),
    // not pushed to the far edge of the row.
    let row = snap
        .lines()
        .find(|l| l.contains("Tip:"))
        .unwrap_or_else(|| panic!("tip row missing:\n{snap}"));
    assert!(
        row.contains("Idle    Tip: # after"),
        "tip must sit one tab (4 cols) behind the status:\n{snap}"
    );
}

#[test]
fn process_row_shows_thinking_word_with_panel_visible() {
    let mut app = App::new();
    app.push_system("hello");
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "analyze ".repeat(120),
        agent_id: None,
        trace_id: None,
    }));
    app.status = AgentStatus::Running {
        phase: Phase::Thinking,
    };
    let snap = snapshot_text(&mut app, 80, 24);
    // The folded thinking panel is up AND the process row still says
    // "thinking..." in full - the panel never replaces the live status word.
    assert!(snap.contains("thinking -"), "panel title missing:\n{snap}");
    assert!(
        snap.contains("thinking..."),
        "status keeps the thinking word while the panel is visible:\n{snap}"
    );
}
