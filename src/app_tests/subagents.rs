//! Sub-agent rendering, child streams, and last-reply copy.

use super::super::*;
use super::common::*;

#[test]
fn sub_agent_text_is_labeled_and_lifecycle_tracked() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "found a thing")));
    // Flush to commit text
    app.flush_pending();
    assert_eq!(
        app.sub_agents.get("root/a").map(|s| &s.status),
        Some(&SubAgentStatus::Running)
    );
    // Child RunFinished is flush-only (ENG-F1 shape B): the row's terminal
    // fact arrives from the watcher wire / registry lifecycle.
    app.handle_event(TuiEvent::Runtime(run_finished(Some("root/a"))));
    assert_eq!(
        app.sub_agents.get("root/a").map(|s| &s.status),
        Some(&SubAgentStatus::Running)
    );
    // Pin (inspection freeze 检视冻结): the sole row's terminal fact triggers the all-complete sweep, and
    // the assertions below need the entry and its child transcript.
    app.task_panel.focus = FocusTarget::TaskList("root/a".to_string());
    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);
    assert_eq!(
        app.sub_agents.get("root/a").map(|s| &s.status),
        Some(&SubAgentStatus::Done)
    );
    // Main transcript: the started marker plus the permanent done ledger line.
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(texts.len(), 2, "started + ledger marker: {texts:?}");
    assert_eq!(texts[0], "* [root/a] started");
    assert!(
        texts[1].starts_with("* [root/a] done ("),
        "permanent ledger marker missing: {texts:?}"
    );
    // Check sub-agent transcript
    let sub_texts: Vec<&str> = app
        .sub_agent_transcripts
        .get("root/a")
        .map(|t| t.iter().map(|l| l.text.as_str()).collect())
        .unwrap_or_default();
    // Sub-agent transcript should have the text and the same done verdict.
    assert!(sub_texts.iter().any(|t| t.contains("found a thing")));
    assert!(
        sub_texts.iter().any(|t| t.starts_with("* [root/a] done (")),
        "child verdict missing: {sub_texts:?}"
    );
}

#[test]
fn sub_agent_tool_calls_are_labeled() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(child_tool_started("root/a", "read_file")));
    app.handle_event(TuiEvent::Runtime(child_tool_finished(
        "root/a",
        "read_file",
    )));
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(texts, vec!["* [root/a] started",]);
    // Check sub-agent transcript
    let sub_texts: Vec<&str> = app
        .sub_agent_transcripts
        .get("root/a")
        .map(|t| t.iter().map(|l| l.text.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(
        sub_texts,
        vec!["* [root/a] read_file", "  [root/a] < done",]
    );
}

#[test]
fn root_finish_sweeps_the_corpses_left_behind() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "hi")));
    app.handle_event(TuiEvent::Runtime(run_finished(Some("root/a"))));
    // A running daemon (dev server) blocks the all-complete sweep at the terminal —
    // auto_sweep counts every running background row …
    app.background_tasks.insert(
        "bg_daemon".to_string(),
        BackgroundTaskEntry {
            id: "bg_daemon".to_string(),
            command: "npm run dev".to_string(),
            timeout_ms: 0,
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            finished_at: None,
            reported: false,
            output_tail: String::new(),
            consumed: false,
        },
    );
    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);
    assert!(
        app.sub_agents.contains_key("root/a"),
        "the daemon blocks the all-complete sweep"
    );

    // … but root settle runs the turn-end sweep (Rev 2026-11-04, principle ②: corpses do not cross
    // turn): the corpse goes and the daemon row stays — a server left
    // running is the product working as intended.
    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    assert!(!app.sub_agents.contains_key("root/a"), "turn-end sweep");
    assert!(app.background_tasks.contains_key("bg_daemon"));
}

#[test]
fn sub_agent_run_finished_does_not_end_turn() {
    let mut app = App::new();
    app.running = true;
    // Root activity establishes the status...
    app.handle_event(TuiEvent::Runtime(text("working")));
    assert!(matches!(app.status, AgentStatus::Running { .. }));
    // ...child streaming and its RunFinished must leave it untouched.
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "hi")));
    app.handle_event(TuiEvent::Runtime(run_finished(Some("root/a"))));
    assert!(app.running);
    assert!(matches!(app.status, AgentStatus::Running { .. }));
    // Even the child's terminal fact (wire-driven) must not end the root turn.
    // The pin (inspection freeze 检视冻结) only keeps the entry readable past its sweep.
    app.task_panel.focus = FocusTarget::TaskList("root/a".to_string());
    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);
    assert!(app.running);
    assert!(matches!(app.status, AgentStatus::Running { .. }));
    assert_eq!(
        app.sub_agents.get("root/a").map(|s| &s.status),
        Some(&SubAgentStatus::Done)
    );
}

#[test]
fn agent_prefix_labels_sub_agents_only() {
    use crate::handlers::runtime::agent_prefix;
    assert_eq!(agent_prefix(None), "");
    assert_eq!(agent_prefix(Some("")), "");
    assert_eq!(agent_prefix(Some("root/a")), "[root/a] ");
}

// ── Child streams stay out of the main view (session 20260904_3eeb5610) ──
//
// A minutes-long child run streamed its TextDelta/ThoughtDelta into the
// shared stream buffer, whose pending tail IS the main view's live tail —
// the user watched it churn at ~10 events/sec. Child deltas must accumulate
// per-child (visible in the child's focus view), never in the main tail.

#[test]
fn child_deltas_bypass_main_stream_tail() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(child_thought("root/a", "thinking hard")));
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "partial prose")));
    // Main live tail: untouched by child deltas.
    assert!(app.streaming_tail_raw().is_none());
    assert!(!app.stream.has_pending_text());
    assert!(!app.stream.has_pending_thought());
    // Main transcript: only the started marker.
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(texts, vec!["* [root/a] started"]);
    // Child content is preserved, committed ahead of the done marker.
    app.handle_event(TuiEvent::Runtime(run_finished(Some("root/a"))));
    // The verdict is written when the terminal fact lands (wire/lifecycle).
    // The pin (inspection freeze 检视冻结) keeps the transcript past the all-complete sweep for the reads.
    app.task_panel.focus = FocusTarget::TaskList("root/a".to_string());
    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);
    let sub_texts: Vec<&str> = app
        .sub_agent_transcripts
        .get("root/a")
        .map(|t| t.iter().map(|l| l.text.as_str()).collect())
        .unwrap_or_default();
    let done_idx = sub_texts.iter().position(|t| t.contains("done")).unwrap();
    assert!(
        sub_texts[..done_idx]
            .iter()
            .any(|t| t.contains("thinking hard")),
        "child thought must reach the child transcript: {sub_texts:?}"
    );
    assert!(
        sub_texts[..done_idx]
            .iter()
            .any(|t| t.contains("partial prose")),
        "child text must reach the child transcript: {sub_texts:?}"
    );
}

#[test]
fn child_stream_flushes_before_its_tool_line() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(child_text(
        "root/a",
        "read the config first",
    )));
    app.handle_event(TuiEvent::Runtime(child_tool_started("root/a", "read_file")));
    let sub_texts: Vec<&str> = app
        .sub_agent_transcripts
        .get("root/a")
        .map(|t| t.iter().map(|l| l.text.as_str()).collect())
        .unwrap_or_default();
    // Pending child text commits above the invocation line, in order.
    assert!(
        sub_texts.len() >= 2
            && sub_texts[0].contains("read the config first")
            && sub_texts[1].starts_with("* [root/a] read_file"),
        "child text must precede its tool line: {sub_texts:?}"
    );
    assert!(app.streaming_tail_raw().is_none());
}

#[test]
fn root_tail_survives_child_interleaving() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(text("hel")));
    // First sight of the child flushes root pending text (started-marker
    // ordering); child deltas themselves must never join the root buffer.
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "child chunk")));
    app.handle_event(TuiEvent::Runtime(text("lo")));
    // Root tail: only root prose — no child content mixed in.
    let (raw, kind) = app.streaming_tail_raw().expect("root tail present");
    assert_eq!(raw, "lo");
    assert_eq!(kind, LineKind::Normal);
    // "hel" was committed whole by the started marker, unchopped.
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert!(
        texts.contains(&"hel"),
        "root text flushed intact: {texts:?}"
    );
    assert!(
        texts.iter().all(|t| !t.contains("child chunk")),
        "child content must stay out of the main transcript: {texts:?}"
    );
}

#[test]
fn sweep_drops_child_stream() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "some prose")));
    app.handle_event(TuiEvent::Runtime(run_finished(Some("root/a"))));
    // A still-running sibling blocks the all-complete sweep, so the buffers survive the
    // terminal fact and the sweep below is the one under test (Rev 2026-11-04:
    // death sweeps, no TTL).
    app.handle_event(TuiEvent::Runtime(child_text("root/keep", "sibling")));
    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);
    assert!(app.child_streams.contains_key("root/a"));
    assert!(app.turn_end_sweep());
    assert!(!app.child_streams.contains_key("root/a"));
    assert!(!app.sub_agent_transcripts.contains_key("root/a"));
    assert!(
        app.sub_agents.contains_key("root/keep"),
        "running rows survive the sweep"
    );
}

#[test]
fn last_reply_text_captures_reply_after_last_user() {
    let mut app = App::new();
    app.push_user("give me a url");
    app.handle_event(TuiEvent::Runtime(text("here: https://example.com/x")));
    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    assert_eq!(app.last_reply_text(), "here: https://example.com/x");
}

#[test]
fn last_reply_text_skips_tools_and_stops_at_previous_user() {
    let mut app = App::new();
    app.push_user("turn one");
    app.handle_event(TuiEvent::Runtime(text("old answer")));
    app.handle_event(TuiEvent::Runtime(run_finished(None)));

    app.push_user("turn two");
    app.handle_event(TuiEvent::Runtime(text("part one")));
    app.handle_event(TuiEvent::Runtime(tool_started("build")));
    app.handle_event(TuiEvent::Runtime(tool_finished("build", false)));
    app.handle_event(TuiEvent::Runtime(text("part two")));
    app.handle_event(TuiEvent::Runtime(run_finished(None)));

    // Only turn two's prose; tool lines, results and the `✅ done` marker
    // are excluded, and the scan stops at the previous user turn.
    assert_eq!(app.last_reply_text(), "part one\npart two");
}

#[test]
fn last_reply_text_empty_without_reply() {
    let app = App::new();
    assert_eq!(app.last_reply_text(), "");
}

#[test]
fn ctrl_y_emits_copy_last_reply_and_plain_y_types() {
    let mut app = App::new();
    assert_eq!(
        app.handle_key(KeyCode::Char('y'), KeyModifiers::CONTROL),
        Some(Action::CopyLastReply)
    );
    // Plain 'y' (no Ctrl) still inserts a literal 'y'.
    assert_eq!(app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "y");
}

#[test]
fn notice_shows_in_status_line_and_clears_on_key() {
    let mut app = App::new();
    assert!(app.status_line().starts_with("Idle"));
    app.set_notice("📋 copied 5 chars");
    assert_eq!(app.status_line(), "📋 copied 5 chars");
    // Any keypress clears the notice before being handled.
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.status_line().starts_with("Idle"));
}
