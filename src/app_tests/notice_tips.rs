//! Notice lines and rotating tips.

use super::super::*;
use super::common::*;
use phi_agent::SessionId;
use phi_agent::UserEvent;

// ── Notice rendering (Batch E, PR4) ─────────────────────────────────────────

fn notice(kind: phi_agent::NoticeKind, source: &str, text: &str) -> RuntimeEvent {
    RuntimeEvent::UserEvent {
        session_id: SessionId::new(1),
        event: UserEvent::Notice {
            kind,
            source: source.to_string(),
            text: text.to_string(),
        },
        agent_id: None,
        trace_id: None,
    }
}

#[test]
fn notice_warning_renders_persistent_error_line() {
    let mut app = App::new();
    // Mid-turn precondition: only then is "does not settle the turn"
    // observable — Idle→Idle proves nothing.
    app.handle_event(TuiEvent::Runtime(tool_started("execute_command")));
    assert!(
        matches!(app.status, AgentStatus::Running { .. }),
        "precondition: mid-turn"
    );
    let lines_before = app.transcript.output.len();

    app.handle_event(TuiEvent::Runtime(notice(
        phi_agent::NoticeKind::Warning,
        "guard",
        "guard judge unparsed — treating as complete",
    )));

    // Warning lands as a red (Error-kind) transcript line — persistent.
    assert_eq!(
        app.transcript.output.len(),
        lines_before + 1,
        "exactly one line emitted"
    );
    let line = app.transcript.output.last().expect("one line");
    assert_eq!(line.kind, LineKind::Error, "warning renders as red line");
    // User-facing copy: the mechanism fact ("guard judge …") never reaches
    // the user — nobody outside the engine knows what a "judge" is.
    assert!(
        line.text.contains("Answer accepted without verification."),
        "line carries the user-facing copy, got: {}",
        line.text
    );
    assert!(
        !line.text.contains("judge") && !line.text.contains("guard"),
        "mechanism jargon must not leak to the user, got: {}",
        line.text
    );
    // No turn settlement: the notice must not call settle_after_turn —
    // status stays Running.
    assert!(
        matches!(app.status, AgentStatus::Running { .. }),
        "warning must not settle the turn, got: {:?}",
        app.status
    );
}

/// The attention kind is the amber twin of the red warning: "look at this"
/// without "this broke" — a task blocked on an interactive prompt.
#[test]
fn notice_attention_renders_amber_not_red() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(tool_started("execute_command")));
    app.handle_event(TuiEvent::Runtime(notice(
        phi_agent::NoticeKind::Attention,
        "tool:execute_command",
        "task bg_x is waiting for input (confirm): Continue? [Y/n]",
    )));

    let line = app.transcript.output.last().expect("one line");
    assert_eq!(
        line.kind,
        LineKind::Notice,
        "an attention notice must not wear the error face"
    );
    assert!(
        line.text.contains("waiting for input"),
        "the task fact reaches the user, got: {}",
        line.text
    );
    assert!(
        matches!(app.status, AgentStatus::Running { .. }),
        "an attention notice must not settle the turn"
    );
}

/// The three judge fail-open causes (unparsed / call failed / unavailable)
/// are indistinguishable to a user and none of them is actionable — product
/// layer collapses them to one copy (design §8: mechanism layer keeps the
/// cause-specific English facts for logs and tests).
#[test]
fn notice_guard_judge_fail_open_causes_collapse_to_one_user_copy() {
    let user_copy = "Answer accepted without verification.";
    for mechanism in [
        "guard judge unparsed — treating as complete",
        "guard judge call failed — treating as complete",
        "guard judge unavailable — treating as complete",
    ] {
        let mut app = App::new();
        app.handle_event(TuiEvent::Runtime(notice(
            phi_agent::NoticeKind::Warning,
            "guard",
            mechanism,
        )));
        let line = app.transcript.output.last().expect("one line");
        assert!(
            line.text.contains(user_copy),
            "mechanism `{mechanism}` must render as the user copy, got: {}",
            line.text
        );
    }

    // Guard notices that are NOT judge fail-open keep their own text.
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(notice(
        phi_agent::NoticeKind::Warning,
        "guard",
        "guard reasoning strikes exhausted",
    )));
    let line = app.transcript.output.last().expect("one line");
    assert!(
        line.text.contains("guard reasoning strikes exhausted"),
        "non-judge guard notices pass through, got: {}",
        line.text
    );
}

#[test]
fn notice_progress_updates_live_progress() {
    let mut app = App::new();
    // live_progress renders only in the Running/ToolCall phase — same as
    // tool Progress (existing semantics, see progress_updates_and_clears…).
    app.handle_event(TuiEvent::Runtime(tool_started("execute_command")));
    app.handle_event(TuiEvent::Runtime(notice(
        phi_agent::NoticeKind::Progress,
        "compactor",
        "compacting context…",
    )));

    // Progress is transient status-bar text — same channel as tool progress.
    let line = app.status_line();
    assert!(
        line.contains("compacting context…"),
        "progress notice surfaces in the status bar, got: {line}"
    );
    let lines_after_progress = app.transcript.output.len();
    // Any further notices/transcript churn must not come from this event:
    // the notice only feeds the status bar.
    app.handle_event(TuiEvent::Runtime(notice(
        phi_agent::NoticeKind::Progress,
        "compactor",
        "still compacting…",
    )));
    assert_eq!(
        app.transcript.output.len(),
        lines_after_progress,
        "progress must not pollute the transcript"
    );
}

#[test]
fn notice_info_renders_plain_line() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(notice(
        phi_agent::NoticeKind::Info,
        "reaper",
        "reaped 2 background tasks",
    )));

    assert_eq!(app.transcript.output.len(), 1, "one line emitted");
    let line = &app.transcript.output[0];
    assert_eq!(
        line.kind,
        LineKind::System,
        "info renders as gray system line"
    );
    assert!(
        line.text.contains("reaped 2 background tasks"),
        "got: {}",
        line.text
    );
}

#[test]
fn tips_rotate_once_per_settled_turn() {
    let mut app = App::new();
    app.set_tips(vec![
        TipItem::tip("one"),
        TipItem::tip("two"),
        TipItem::tip("three"),
    ]);
    assert_eq!(app.current_tip().map(|t| t.text.as_str()), Some("one"));

    // A settled turn advances the rotation exactly once ...
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    app.handle_event(TuiEvent::TurnDone);
    assert_eq!(app.status, AgentStatus::Idle);
    assert_eq!(
        app.current_tip().map(|t| t.text.as_str()),
        Some("two"),
        "one settled turn = one step"
    );

    // ... and a settle with no fresh stretch must not skip entries.
    app.handle_event(TuiEvent::TurnDone);
    assert_eq!(
        app.current_tip().map(|t| t.text.as_str()),
        Some("two"),
        "repeated settle keeps the entry"
    );

    // Round-robin wraps without repeats within a pass.
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    app.handle_event(TuiEvent::TurnDone);
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    app.handle_event(TuiEvent::TurnDone);
    assert_eq!(
        app.current_tip().map(|t| t.text.as_str()),
        Some("one"),
        "wraps around after the last entry"
    );
}

#[test]
fn tip_visible_idle_and_after_activity_threshold() {
    let mut app = App::new();
    app.set_tips(vec![TipItem::tip("t")]);
    // Idle: the tips line is resident.
    assert!(app.tip_visible());

    // A fresh active stretch hides it until the threshold ...
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    assert!(app.is_active());
    assert!(!app.tip_visible());

    // ... and a long-running action past the threshold shows it again.
    app.activity_since = Some(Instant::now() - std::time::Duration::from_secs(10));
    assert!(app.tip_visible());
}

#[test]
fn tip_hidden_without_items() {
    let app = App::new();
    assert!(app.tips.is_empty());
    assert!(app.current_tip().is_none());
    assert!(!app.tip_visible(), "no pool renders no tips line");
}

#[test]
fn tips_updated_event_replaces_pool_and_resets_rotation() {
    let mut app = App::new();
    app.set_tips(vec![
        TipItem::tip("built-in one"),
        TipItem::tip("built-in two"),
    ]);
    app.rotate_tip();
    assert_eq!(
        app.current_tip().map(|t| t.text.as_str()),
        Some("built-in two")
    );

    app.handle_event(TuiEvent::TipsUpdated {
        items: vec![TipItem::note(
            "phimint is built on phi-agent - docs: https://docs.phiagent.dev/",
        )],
    });
    let tip = app.current_tip().expect("cloud pool renders");
    assert_eq!(tip.kind, crate::tips::TipKind::Note);
    assert_eq!(
        tip.text,
        "phimint is built on phi-agent - docs: https://docs.phiagent.dev/"
    );
    assert_eq!(app.tip_index, 0, "a fresh pool restarts the rotation");
}
