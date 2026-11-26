//! Mock tests for the multi-task panel feature.
//!
//! These tests simulate sub-agent lifecycles without requiring real LLM calls,
//! verifying the task panel state, transcript routing, and cleanup behavior.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::*;
use crate::app::{App, FocusTarget, SubAgentState, SubAgentStatus, ToolEvent};
use phi_agent::{RuntimeEvent, SessionId};
use phi_tui::lines::{LineKind, OutputLine};

// ── Mock Event Builders ──────────────────────────────────────────────────────

/// Create a TextDelta event for a sub-agent.
fn sub_text(agent_id: &str, text: &str) -> RuntimeEvent {
    RuntimeEvent::TextDelta {
        session_id: SessionId::new(1),
        text: text.to_string(),
        agent_id: Some(agent_id.to_string()),
        trace_id: None,
    }
}

/// Create a ThoughtDelta event for a sub-agent.
fn sub_thought(agent_id: &str, text: &str) -> RuntimeEvent {
    RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: text.to_string(),
        agent_id: Some(agent_id.to_string()),
        trace_id: None,
    }
}

/// Create a ActionStarted event for a sub-agent.
fn sub_tool_started(agent_id: &str, tool_name: &str) -> RuntimeEvent {
    RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: tool_name.to_string(),
        args_json: "{}".to_string(),
        agent_id: Some(agent_id.to_string()),
        trace_id: None,
    }
}

/// Create a ActionFinished event for a sub-agent.
fn sub_tool_finished(agent_id: &str, tool_name: &str, summary: &str) -> RuntimeEvent {
    RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: tool_name.to_string(),
        summary: summary.to_string(),
        agent_id: Some(agent_id.to_string()),
        trace_id: None,
        denied: false,
        details: None,
    }
}

/// Create a RunFinished event for a sub-agent.
fn sub_run_finished(agent_id: &str) -> RuntimeEvent {
    RuntimeEvent::RunFinished {
        session_id: SessionId::new(1),
        agent_id: Some(agent_id.to_string()),
        trace_id: None,
    }
}

// ── Test Helpers ─────────────────────────────────────────────────────────────

/// Helper to create a SubAgentState with minimal fields.
fn mock_sub_agent(name: &str, status: SubAgentStatus) -> SubAgentState {
    SubAgentState {
        name: name.to_string(),
        status,
        files: vec![format!("src/{name}.rs")],
        started_at: Instant::now(),
        completed_at: None,
        last_tool_at: Instant::now(),
        events: Vec::new(),
    }
}

/// Helper to create a completed SubAgentState.
fn mock_completed_agent(name: &str, completed_secs_ago: u64) -> SubAgentState {
    SubAgentState {
        name: name.to_string(),
        status: SubAgentStatus::Done,
        files: vec![format!("src/{name}.rs")],
        started_at: Instant::now() - Duration::from_secs(completed_secs_ago + 5),
        completed_at: Some(Instant::now() - Duration::from_secs(completed_secs_ago)),
        last_tool_at: Instant::now() - Duration::from_secs(completed_secs_ago),
        events: Vec::new(),
    }
}

/// Insert a mock sub-agent into the app.
fn insert_mock_agent(app: &mut App, agent_id: &str, name: &str, status: SubAgentStatus) {
    app.sub_agents
        .insert(agent_id.to_string(), mock_sub_agent(name, status));
}

/// Insert a completed agent with a specific completion time.
fn insert_completed_agent(app: &mut App, agent_id: &str, name: &str, completed_secs_ago: u64) {
    app.sub_agents.insert(
        agent_id.to_string(),
        mock_completed_agent(name, completed_secs_ago),
    );
}

// ── Task Panel State Tests ───────────────────────────────────────────────────

#[test]
fn task_panel_focus_navigation() {
    let mut app = App::new();

    // Insert mock agents in a known spawn order (row order is spawn order;
    // backdate started_at so it cannot tie).
    for (id, name, secs_ago) in [
        ("root/auth", "auth", 3),
        ("root/cache", "cache", 2),
        ("root/db", "db", 1),
    ] {
        insert_mock_agent(&mut app, id, name, SubAgentStatus::Running);
        app.sub_agents.get_mut(id).unwrap().started_at =
            Instant::now() - Duration::from_secs(secs_ago);
    }

    // Default focus is Input
    assert_eq!(app.task_panel.focus, FocusTarget::Input);

    // Up from Input opens the frame and lands on the last row (D10).
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        app.task_panel.focus,
        FocusTarget::TaskList("root/db".to_string())
    );

    // Up walks up the spawn-ordered list (selection lands by id — rows
    // vanish and indices shift, Rev 2026-11-04).
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        app.task_panel.focus,
        FocusTarget::TaskList("root/cache".to_string())
    );
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        app.task_panel.focus,
        FocusTarget::TaskList("root/auth".to_string())
    );

    // Up at the first row stays at the first.
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        app.task_panel.focus,
        FocusTarget::TaskList("root/auth".to_string())
    );

    // Down walks back down the list.
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        app.task_panel.focus,
        FocusTarget::TaskList("root/cache".to_string())
    );
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        app.task_panel.focus,
        FocusTarget::TaskList("root/db".to_string())
    );

    // Down past the full-list last row drops to S1 (Input).
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.task_panel.focus, FocusTarget::Input);
}

// ── Transcript Routing Tests ─────────────────────────────────────────────────

#[test]
fn sub_agent_text_routes_to_transcript() {
    let mut app = App::new();

    // Send text from sub-agent
    app.handle_event(TuiEvent::Runtime(sub_text(
        "root/auth",
        "analyzing auth module",
    )));

    // Main transcript should only have the "started" marker
    let main_texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert!(
        main_texts
            .iter()
            .all(|t| !t.contains("analyzing auth module"))
    );

    // Child deltas bypass the shared stream (whose pending tail is the main
    // view's live tail) — they commit on the child's lifecycle events. The
    // RunFinished arm is flush-only (ENG-F1 shape B): the done verdict comes
    // with the terminal fact.
    // Pin the row first: the terminal fact triggers the all-complete sweep (nothing else
    // is running), and the pinned row (inspection freeze 检视冻结) is what keeps its transcript
    // readable here.
    app.task_panel.focus = FocusTarget::TaskList("root/auth".to_string());
    app.handle_event(TuiEvent::Runtime(sub_run_finished("root/auth")));
    app.mark_sub_agent_finished("root/auth", SubAgentStatus::Done);

    // Should be in sub_agent_transcripts, ahead of the done marker
    let sub_lines = app.sub_agent_transcripts.get("root/auth").unwrap();
    assert_eq!(sub_lines.len(), 2);
    // Note: stream adds [agent_id] prefix to text
    assert!(sub_lines[0].text.contains("analyzing auth module"));
    assert_eq!(sub_lines[0].kind, LineKind::Normal);
    assert!(sub_lines[1].text.contains("done"));
}

#[test]
fn sub_agent_thought_routes_to_transcript() {
    let mut app = App::new();

    // Send thought from sub-agent
    app.handle_event(TuiEvent::Runtime(sub_thought(
        "root/auth",
        "thinking about security",
    )));

    // Child thought pends in the per-child stream — never the shared one.
    assert!(!app.stream.has_pending_thought());
    app.handle_event(TuiEvent::Runtime(sub_run_finished("root/auth")));

    let sub_lines = app.sub_agent_transcripts.get("root/auth").unwrap();
    assert!(!sub_lines.is_empty());
    assert_eq!(sub_lines[0].kind, LineKind::Thought);
}

#[test]
fn sub_agent_tool_calls_route_to_transcript() {
    let mut app = App::new();

    // Simulate tool call lifecycle
    app.handle_event(TuiEvent::Runtime(sub_tool_started(
        "root/auth",
        "read_file",
    )));
    app.handle_event(TuiEvent::Runtime(sub_tool_finished(
        "root/auth",
        "read_file",
        "120 lines",
    )));

    // Should be in sub_agent_transcripts
    let sub_lines = app.sub_agent_transcripts.get("root/auth").unwrap();
    assert_eq!(sub_lines.len(), 2);
    assert!(sub_lines[0].text.contains("read_file"));
    assert!(sub_lines[1].text.contains("<"));
}

#[test]
fn sub_agent_completion_routes_to_transcript() {
    let mut app = App::new();

    // Simulate complete lifecycle — the terminal fact arrives from the wire.
    // The pin (inspection freeze 检视冻结) keeps the entry from being swept before we read it.
    app.handle_event(TuiEvent::Runtime(sub_text("root/auth", "work done")));
    app.task_panel.focus = FocusTarget::TaskList("root/auth".to_string());
    app.handle_event(TuiEvent::Runtime(sub_run_finished("root/auth")));
    app.mark_sub_agent_finished("root/auth", SubAgentStatus::Done);

    // Check sub-agent state
    let state = app.sub_agents.get("root/auth").unwrap();
    assert_eq!(state.status, SubAgentStatus::Done);
    assert!(state.completed_at.is_some());

    // Check transcript
    let sub_lines = app.sub_agent_transcripts.get("root/auth").unwrap();
    assert!(sub_lines.last().unwrap().text.contains("done"));
}

// ── Death sweeps (Rev 2026-11-04) ─────────────────────────────────────────

#[test]
fn sweep_removes_terminal_rows_but_spares_running_ones() {
    let mut app = App::new();

    // Age is not a condition any more (the old TTL cleanup is retired): the
    // sweep IS the death — turn end / all-complete take every terminal row the
    // display set no longer owes, however recently it landed.
    insert_mock_agent(&mut app, "root/running", "running", SubAgentStatus::Running);
    insert_completed_agent(&mut app, "root/recent", "recent", 1);
    insert_completed_agent(&mut app, "root/old", "old", 4);

    assert!(app.turn_end_sweep());

    // Running rows survive; both terminal rows go regardless of age.
    assert!(app.sub_agents.contains_key("root/running"));
    assert!(!app.sub_agents.contains_key("root/recent"));
    assert!(!app.sub_agents.contains_key("root/old"));
}

#[test]
fn sweep_removes_corresponding_transcripts() {
    let mut app = App::new();

    // A swept row takes its per-child buffers with it.
    insert_completed_agent(&mut app, "root/auth", "auth", 4);
    app.sub_agent_transcripts.insert(
        "root/auth".to_string(),
        vec![OutputLine {
            text: "test".to_string(),
            kind: LineKind::Normal,
            spans: None,
            original: None,
            detail: None,
            tool_state: None,
        }],
    );

    app.turn_end_sweep();

    assert!(!app.sub_agent_transcripts.contains_key("root/auth"));
}

#[test]
fn sweep_does_nothing_when_everything_is_running() {
    let mut app = App::new();

    // Insert only running agents
    insert_mock_agent(&mut app, "root/a", "a", SubAgentStatus::Running);
    insert_mock_agent(&mut app, "root/b", "b", SubAgentStatus::Running);

    assert!(!app.turn_end_sweep());
    assert!(!app.auto_sweep());
    assert_eq!(app.sub_agents.len(), 2);
}

// ── Panel Visibility Tests ───────────────────────────────────────────────────

#[test]
fn panel_visible_when_agents_exist() {
    let mut app = App::new();
    assert!(!app.should_show_task_panel());

    insert_mock_agent(&mut app, "root/auth", "auth", SubAgentStatus::Running);
    assert!(app.should_show_task_panel());
}

#[test]
fn panel_hidden_after_all_agents_removed() {
    let mut app = App::new();

    // Insert and then remove agent
    insert_mock_agent(&mut app, "root/auth", "auth", SubAgentStatus::Running);
    assert!(app.should_show_task_panel());

    app.sub_agents.clear();
    assert!(!app.should_show_task_panel());
}

// ── Multiple Sub-Agents Tests ────────────────────────────────────────────────

#[test]
fn multiple_sub_agents_isolated_transcripts() {
    let mut app = App::new();

    // Simulate two sub-agents working in parallel
    // Need to flush between agents to ensure proper routing
    app.handle_event(TuiEvent::Runtime(sub_text("root/auth", "auth work")));
    app.flush_pending();
    app.handle_event(TuiEvent::Runtime(sub_tool_started(
        "root/auth",
        "read_file",
    )));

    app.handle_event(TuiEvent::Runtime(sub_text("root/cache", "cache work")));
    app.flush_pending();
    app.handle_event(TuiEvent::Runtime(sub_tool_started(
        "root/cache",
        "write_file",
    )));

    app.handle_event(TuiEvent::Runtime(sub_tool_finished(
        "root/auth",
        "read_file",
        "done",
    )));
    app.handle_event(TuiEvent::Runtime(sub_tool_finished(
        "root/cache",
        "write_file",
        "done",
    )));
    app.handle_event(TuiEvent::Runtime(sub_run_finished("root/auth")));
    app.handle_event(TuiEvent::Runtime(sub_run_finished("root/cache")));

    // Check isolated transcripts
    let auth_lines = app.sub_agent_transcripts.get("root/auth").unwrap();
    let cache_lines = app.sub_agent_transcripts.get("root/cache").unwrap();

    // Auth transcript should only contain auth-related content
    assert!(auth_lines.iter().all(|l| !l.text.contains("cache work")));
    assert!(auth_lines.iter().any(|l| l.text.contains("auth work")));
    assert!(auth_lines.iter().any(|l| l.text.contains("read_file")));

    // Cache transcript should only contain cache-related content
    assert!(cache_lines.iter().all(|l| !l.text.contains("auth work")));
    assert!(cache_lines.iter().any(|l| l.text.contains("cache work")));
    assert!(cache_lines.iter().any(|l| l.text.contains("write_file")));
}

#[test]
fn multiple_sub_agents_events_tracked() {
    let mut app = App::new();

    // Simulate tool calls for multiple agents
    app.handle_event(TuiEvent::Runtime(sub_tool_started(
        "root/auth",
        "read_file",
    )));
    app.handle_event(TuiEvent::Runtime(sub_tool_started(
        "root/cache",
        "write_file",
    )));
    app.handle_event(TuiEvent::Runtime(sub_tool_finished(
        "root/auth",
        "read_file",
        "done",
    )));
    app.handle_event(TuiEvent::Runtime(sub_tool_finished(
        "root/cache",
        "write_file",
        "done",
    )));

    // Check events in SubAgentState
    let auth_state = app.sub_agents.get("root/auth").unwrap();
    let cache_state = app.sub_agents.get("root/cache").unwrap();

    assert_eq!(auth_state.events.len(), 1);
    assert_eq!(auth_state.events[0].tool_name, "read_file");
    assert!(auth_state.events[0].is_finished);

    assert_eq!(cache_state.events.len(), 1);
    assert_eq!(cache_state.events[0].tool_name, "write_file");
    assert!(cache_state.events[0].is_finished);
}

// ── Integration Test: Full Lifecycle ─────────────────────────────────────────

#[test]
fn full_sub_agent_lifecycle() {
    let mut app = App::new();

    // 1. Sub-agent starts with text
    app.handle_event(TuiEvent::Runtime(sub_text(
        "root/auth",
        "starting analysis",
    )));
    assert!(app.should_show_task_panel());
    assert_eq!(
        app.sub_agents.get("root/auth").unwrap().status,
        SubAgentStatus::Running
    );

    // 2. Sub-agent does tool calls
    app.handle_event(TuiEvent::Runtime(sub_tool_started(
        "root/auth",
        "read_file",
    )));
    app.handle_event(TuiEvent::Runtime(sub_tool_finished(
        "root/auth",
        "read_file",
        "120 lines",
    )));

    // 3. It finishes — the terminal fact arrives from the wire. Nothing else
    //    is running and nothing is pinned, so hide-on-complete and the all-complete sweep
    //    sweep land together (Rev 2026-11-04): the row and its buffers go
    //    at once — no TTL, no review window.
    app.handle_event(TuiEvent::Runtime(sub_run_finished("root/auth")));
    app.mark_sub_agent_finished("root/auth", SubAgentStatus::Done);

    // 4. The face is empty and the store is swept …
    assert!(!app.should_show_task_panel());
    assert!(app.sub_agents.is_empty());
    assert!(app.sub_agent_transcripts.is_empty());

    // 5. … but the ledger line is the permanent record (grep-able after the
    //    row is swept).
    assert!(
        app.transcript
            .output
            .iter()
            .any(|l| l.text.contains("* [root/auth] done (")),
        "ledger line must outlive the row: {:?}",
        app.transcript
            .output
            .iter()
            .map(|l| &l.text)
            .collect::<Vec<_>>()
    );
}

// ── Edge Cases ───────────────────────────────────────────────────────────────

#[test]
fn empty_agent_id_ignored() {
    let mut app = App::new();

    // Send text with empty agent_id (should be treated as root)
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::TextDelta {
        session_id: SessionId::new(1),
        text: "root text".to_string(),
        agent_id: Some("".to_string()),
        trace_id: None,
    }));

    // Should be in main transcript (since empty agent_id is treated as root)
    // Note: track_agent ignores empty agent_id, so text goes to main transcript
    // but might be buffered in stream
    if app.stream.has_pending_text() {
        // Text is in stream buffer, which is expected
        // It will be flushed on the next structural event
    } else {
        // Text was flushed to main transcript
        assert!(
            app.transcript
                .output
                .iter()
                .any(|l| l.text.contains("root text"))
        );
    }
    assert!(app.sub_agent_transcripts.is_empty());
}

#[test]
fn duplicate_agent_id_merges() {
    let mut app = App::new();

    // Send multiple events from same agent, with the child's tool call as
    // the flush point between them (child deltas commit on child lifecycle
    // events, not on the root's flush_pending).
    app.handle_event(TuiEvent::Runtime(sub_text("root/auth", "first")));
    app.handle_event(TuiEvent::Runtime(sub_tool_started(
        "root/auth",
        "read_file",
    )));
    app.handle_event(TuiEvent::Runtime(sub_text("root/auth", "second")));
    // The pin (inspection freeze 检视冻结) keeps the transcript alive past the terminal fact —
    // the all-complete sweep would otherwise take it before the ordering assertion.
    app.task_panel.focus = FocusTarget::TaskList("root/auth".to_string());
    app.handle_event(TuiEvent::Runtime(sub_run_finished("root/auth")));
    // The done verdict is written with the terminal fact (wire).
    app.mark_sub_agent_finished("root/auth", SubAgentStatus::Done);

    // All in the same transcript, in order
    let lines = app.sub_agent_transcripts.get("root/auth").unwrap();
    assert_eq!(lines.len(), 4);
    // Note: stream adds [agent_id] prefix to text
    assert!(lines[0].text.contains("first"));
    assert!(lines[1].text.contains("read_file"));
    assert!(lines[2].text.contains("second"));
    assert!(lines[3].text.contains("done"));
}

// ── Panel Rendering (activity column + timer freeze, 20260904 UX batch) ─────

/// Insert a SubAgentState with a preset tool event.
fn insert_agent_with_event(app: &mut App, agent_id: &str, name: &str, tool: &str, finished: bool) {
    let mut state = mock_sub_agent(name, SubAgentStatus::Running);
    state.events.push(ToolEvent {
        tool_name: tool.to_string(),
        summary: String::new(),
        is_finished: finished,
    });
    app.sub_agents.insert(agent_id.to_string(), state);
}

#[test]
fn panel_shows_frozen_time_for_done_agents() {
    let mut app = App::new();
    let mut state = mock_completed_agent("auth", 10);
    // Started 65 s ago, finished 10 s ago → final runtime 55 s, frozen.
    state.started_at = Instant::now() - Duration::from_secs(65);
    state.completed_at = Some(Instant::now() - Duration::from_secs(10));
    app.sub_agents.insert("root/auth".to_string(), state);
    // Touched → the done row lingers as a dim corpse; an untouched terminal
    // row hides the moment it lands (hide-on-complete 完成即隐).
    app.task_panel.touched.insert("root/auth".to_string());
    app.task_panel.frame_open = true;

    let snap = crate::render::snapshot_text(&mut app, 100, 30);
    // The time column is gone (design mock): a done row freezes its runtime
    // in the activity cell.
    assert!(
        snap.contains("done 55s"),
        "done agent's runtime must freeze at completed-started, got:\n{snap}"
    );
}

#[test]
fn panel_activity_column_shows_latest_tool() {
    let mut app = App::new();
    insert_agent_with_event(&mut app, "root/auth", "auth", "read_file", false);
    app.task_panel.frame_open = true;

    let snap = crate::render::snapshot_text(&mut app, 100, 30);
    assert!(
        snap.contains("> read_file"),
        "panel must show the in-flight tool, got:\n{snap}"
    );

    // Finished → the checkmark form.
    app.handle_event(TuiEvent::Runtime(sub_tool_finished(
        "root/auth",
        "read_file",
        "10 lines",
    )));
    let snap = crate::render::snapshot_text(&mut app, 100, 30);
    assert!(
        snap.contains("+ read_file"),
        "finished tool must show as +, got:\n{snap}"
    );
}

#[test]
fn done_receipt_row_keeps_its_ledger_marker_and_never_spins() {
    let mut app = App::new();
    let mut state = mock_sub_agent("auth", SubAgentStatus::Running);
    state.started_at = Instant::now() - Duration::from_secs(65);
    app.sub_agents.insert("root/auth".to_string(), state);
    app.mark_sub_agent_finished("root/auth", SubAgentStatus::Done);

    let snap = crate::render::snapshot_text(&mut app, 100, 30);
    // The receipt row is a permanent ledger marker, grep-able after the panel
    // row is swept: its `*` is content, not a state-glyph slot. A done row
    // wearing the braille spinner is a display bug (live 20261126: every
    // `done (…)` receipt spun forever because a `tool_state: None` marker
    // row fell into the live-call glyph path).
    let row = snap
        .lines()
        .find(|l| l.contains("] done ("))
        .expect("a done receipt row");
    assert!(
        row.trim_start().starts_with("* [root/auth] done ("),
        "the receipt must keep its ledger marker, got: {row:?}\n{snap}"
    );
    for frame in ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"] {
        assert!(
            !row.contains(frame),
            "a done receipt must not spin, got: {row:?}\n{snap}"
        );
    }
}

#[test]
fn panel_columns_align_with_long_names() {
    let mut app = App::new();
    insert_agent_with_event(
        &mut app,
        "root/analyze-deepseek-harness",
        "analyze-deepseek-harness",
        "read_file",
        false,
    );
    insert_agent_with_event(&mut app, "root/pi", "pi", "read_file", false);
    app.task_panel.frame_open = true;

    let snap = crate::render::snapshot_text(&mut app, 100, 30);
    // #13 任务列宽: the name column widened to 28, so this 25-column name now
    // fits whole instead of being ellipsized at the old 16-column cap.
    assert!(snap.contains("analyze-deepseek-harness"), "got:\n{snap}");
    // Both rows' files column must start at the same character position. The
    // columns are separated by whitespace alone (规则线不入列), so the anchor
    // is the cell's own text — same isolation, no rule characters.
    let cols: Vec<usize> = snap
        .lines()
        .filter(|l| l.contains("analyze-deepseek-harness") || l.contains(" pi "))
        .filter_map(|l| l.find("1 file"))
        .collect();
    assert_eq!(cols.len(), 2, "both panel rows expected, got:\n{snap}");
    assert_eq!(cols[0], cols[1], "columns must align, got:\n{snap}");
}

#[test]
fn panel_columns_align_across_cjk_and_ascii_names() {
    let mut app = App::new();
    // 11 CJK chars = 22 display columns, so it hits the 16-column name cap and
    // must be elided by width. Laid out by char count, one CJK char counts as
    // one cell while occupying two, and every later column slides right.
    insert_agent_with_event(
        &mut app,
        "root/analyze",
        "分析设计文档的实现方案",
        "read_file",
        false,
    );
    insert_agent_with_event(&mut app, "root/pi", "pi", "read_file", false);
    app.task_panel.frame_open = true;

    let snap = crate::render::snapshot_text(&mut app, 100, 30);
    // Both rows' files column must sit on the same DISPLAY column. Comparing
    // char indices would hide the misalignment, so measure the display width
    // of the prefix before the cell's text. The columns are separated by
    // whitespace alone (规则线不入列) — the anchor is the cell text, not a
    // rule character.
    let cols: Vec<usize> = snap
        .lines()
        .filter(|l| l.contains("read_file"))
        .filter_map(|l| {
            let idx = l.find("1 file")?;
            Some(unicode_width::UnicodeWidthStr::width(&l[..idx]))
        })
        .collect();
    assert_eq!(cols.len(), 2, "both panel rows expected, got:\n{snap}");
    assert!(
        cols.iter().all(|&c| c > 1),
        "measured the name column, not the files column: {cols:?}\n{snap}"
    );
    assert_eq!(
        cols[0], cols[1],
        "columns must align in display columns, got:\n{snap}"
    );
}

// ── Rule-free columns + flanked title (20261126 UX batch) ────────────────────

#[test]
fn panel_columns_carry_no_rule_characters() {
    let mut app = App::new();
    insert_agent_with_event(&mut app, "root/auth", "auth", "read_file", false);
    app.task_panel.frame_open = true;

    let snap = crate::render::snapshot_text(&mut app, 100, 30);
    // 规则线不入列: the columns are held apart by whitespace and their own
    // padding — a `│` on a task row would be content the layout invented.
    let rows: Vec<&str> = snap.lines().filter(|l| l.contains("auth")).collect();
    assert!(!rows.is_empty(), "a task row expected:\n{snap}");
    for row in &rows {
        assert!(
            !row.contains('│'),
            "task rows must be rule-free: {row:?}\n{snap}"
        );
    }
}

#[test]
fn panel_title_and_hint_ride_the_rule_flanked() {
    let mut app = App::new();
    insert_agent_with_event(&mut app, "root/auth", "auth", "read_file", false);
    app.task_panel.frame_open = true;

    let snap = crate::render::snapshot_text(&mut app, 100, 30);
    let rule = snap
        .lines()
        .find(|l| l.contains("Tasks (1)"))
        .unwrap_or_else(|| panic!("the title must ride the top rule:\n{snap}"));
    // 横条夹字，不顶两端: each label is flanked by rule stubs and inset from
    // the panel's endpoints — `─ Tasks (1) ───…── ^T close ─`.
    assert!(
        rule.trim_start().starts_with("─ Tasks (1)"),
        "the title needs a rule stub before it: {rule:?}\n{snap}"
    );
    assert!(
        rule.trim_end().ends_with("^T close ─"),
        "the close hint needs a rule stub after it: {rule:?}\n{snap}"
    );
}

#[test]
fn panel_columns_spread_with_the_frame_width() {
    let mut app = App::new();
    insert_agent_with_event(&mut app, "root/auth", "auth", "read_file", false);
    app.task_panel.frame_open = true;

    // 列随框走: the gap between columns widens with the frame, so a wide
    // panel does not read as one left-packed clump.
    let mut files_col = |width: u16| -> usize {
        let snap = crate::render::snapshot_text(&mut app, width, 30);
        snap.lines()
            .find(|l| l.contains("auth"))
            .and_then(|l| l.find("1 file"))
            .unwrap_or_else(|| panic!("files cell expected at {width} cols:\n{snap}"))
    };
    let narrow = files_col(70);
    let wide = files_col(150);
    assert!(
        wide > narrow,
        "the files column must move right with the frame: narrow {narrow}, wide {wide}"
    );
}

// ── Visibility Guarantees (session 20260903 fan-in) ──────────────────────────
// The root turn ends while children still run (the fan-in norm), so between
// turns child events keep flowing through the persistent bus subscription.
// They must not clobber the root's Waiting status, and finished children's
// transcripts must survive while any sibling still runs — the sweep is
// state-based (the all-complete sweep fires when nothing is running, no TTL).

#[test]
fn the_sweep_never_runs_while_siblings_still_run() {
    let mut app = App::new();

    // A child finished while its sibling is still working: the all-complete sweep is
    // state-based (nothing running), so the corpse — and its transcript,
    // the only record of what the sub-agent did — survive the wait.
    insert_completed_agent(&mut app, "root/auth", "auth", 10);
    app.sub_agent_transcripts.insert(
        "root/auth".to_string(),
        vec![OutputLine {
            text: "partial progress".to_string(),
            kind: LineKind::Normal,
            spans: None,
            original: None,
            detail: None,
            tool_state: None,
        }],
    );
    insert_mock_agent(&mut app, "root/db", "db", SubAgentStatus::Running);

    assert!(!app.auto_sweep());
    assert!(app.sub_agents.contains_key("root/auth"));
    assert!(app.sub_agent_transcripts.contains_key("root/auth"));

    // The last sibling lands: now the sweep is due and takes both entries
    // with their buffers.
    app.mark_sub_agent_finished("root/db", SubAgentStatus::Done);
    assert!(
        !app.sub_agents.contains_key("root/auth"),
        "all-complete sweep"
    );
    assert!(!app.sub_agents.contains_key("root/db"));
    assert!(!app.sub_agent_transcripts.contains_key("root/auth"));
}

#[test]
fn child_streaming_does_not_clobber_waiting_status() {
    let mut app = App::new();

    // Root turn ended, two children still running.
    app.status = AgentStatus::Waiting { running: 2, bg: 0 };
    insert_mock_agent(&mut app, "root/auth", "auth", SubAgentStatus::Running);

    // Child text/thought/tool events arrive via the persistent subscription.
    app.handle_event(TuiEvent::Runtime(sub_text("root/auth", "reading files")));
    app.handle_event(TuiEvent::Runtime(sub_tool_started(
        "root/auth",
        "read_file",
    )));
    app.handle_event(TuiEvent::Runtime(sub_tool_finished(
        "root/auth",
        "read_file",
        "10 lines",
    )));

    // Root status must stay Waiting; only root events drive it.
    assert_eq!(
        app.status,
        AgentStatus::Waiting { running: 2, bg: 0 },
        "child events must not flip the root back to Running"
    );

    // ...but the child's activity still lands in its transcript.
    let sub_lines = app.sub_agent_transcripts.get("root/auth").unwrap();
    assert!(sub_lines.iter().any(|l| l.text.contains("read_file")));
}

// ── Focused-child live tail (10.1 backlog batch) ─────────────────────────────
// F3 demotes child stream text to per-child buffers flushed only at tool-call
// boundaries. The focused child view must still render the UNFLUSHED tail —
// the long silent report-writing stretch has no tool boundary to flush at
// (session 20260905_913766db: pi wrote 3.5 invisible minutes).

#[test]
fn focused_child_tail_raw_is_prefixed_and_live() {
    let mut app = App::new();
    insert_mock_agent(&mut app, "root/pi", "pi", SubAgentStatus::Running);

    app.handle_event(TuiEvent::Runtime(sub_text(
        "root/pi",
        "# Report\n\nfindings",
    )));
    let (raw, kind) = app
        .child_stream_tail_raw("root/pi")
        .expect("pending text is the live tail");
    assert_eq!(kind, LineKind::Normal);
    assert_eq!(raw, "[root/pi] # Report\n\nfindings");
}

#[test]
fn focused_child_tail_disappears_at_tool_boundary_flush() {
    let mut app = App::new();
    insert_mock_agent(&mut app, "root/pi", "pi", SubAgentStatus::Running);

    app.handle_event(TuiEvent::Runtime(sub_text("root/pi", "now examining")));
    assert!(app.child_stream_tail_raw("root/pi").is_some());

    // The tool call flushes the pending text into the transcript.
    app.handle_event(TuiEvent::Runtime(sub_tool_started("root/pi", "read_file")));
    assert!(app.child_stream_tail_raw("root/pi").is_none());
    assert!(
        app.sub_agent_transcripts
            .get("root/pi")
            .unwrap()
            .iter()
            .any(|l| l.text.contains("now examining"))
    );
}

#[test]
fn focused_child_thought_tail_uses_wrapped_lines() {
    let mut app = App::new();
    insert_mock_agent(&mut app, "root/pi", "pi", SubAgentStatus::Running);

    app.handle_event(TuiEvent::Runtime(sub_thought("root/pi", "thinking hard")));
    let (lines, kind) = app
        .child_stream_tail_lines("root/pi")
        .expect("pending thought is the live tail");
    assert_eq!(kind, LineKind::Thought);
    assert_eq!(lines[0], "[root/pi] thinking hard");
    // raw accessor reports the same pending content with the thought kind
    assert_eq!(
        app.child_stream_tail_raw("root/pi").map(|(_, k)| k),
        Some(LineKind::Thought)
    );
}

// ── writing... hint (10.1 backlog batch) ─────────────────────────────────────

#[test]
fn writing_hint_shows_for_quiet_running_agent() {
    let mut state = mock_sub_agent("pi", SubAgentStatus::Running);
    state.events.push(ToolEvent {
        tool_name: "read_file".to_string(),
        summary: String::new(),
        is_finished: true,
    });
    state.last_tool_at = Instant::now() - Duration::from_secs(21);
    assert!(is_writing_hint(&state, Instant::now()));
}

#[test]
fn writing_hint_suppressed_for_fresh_tool_or_inflight_or_done() {
    // Fresh finished tool: show the honest + tool_name.
    let mut fresh = mock_sub_agent("pi", SubAgentStatus::Running);
    fresh.events.push(ToolEvent {
        tool_name: "read_file".to_string(),
        summary: String::new(),
        is_finished: true,
    });
    fresh.last_tool_at = Instant::now() - Duration::from_secs(2);
    assert!(!is_writing_hint(&fresh, Instant::now()));

    // In-flight tool: `→ tool` is more accurate than the hint, however long.
    let mut inflight = mock_sub_agent("pi", SubAgentStatus::Running);
    inflight.events.push(ToolEvent {
        tool_name: "repo_map".to_string(),
        summary: String::new(),
        is_finished: false,
    });
    inflight.last_tool_at = Instant::now() - Duration::from_secs(300);
    assert!(!is_writing_hint(&inflight, Instant::now()));

    // Done agents keep their frozen final activity.
    let mut done = mock_completed_agent("pi", 60);
    done.events.push(ToolEvent {
        tool_name: "read_file".to_string(),
        summary: String::new(),
        is_finished: true,
    });
    assert!(!is_writing_hint(&done, Instant::now()));
}

#[test]
fn tool_events_refresh_last_tool_at() {
    let mut app = App::new();
    insert_mock_agent(&mut app, "root/pi", "pi", SubAgentStatus::Running);
    // Backdate, then let real events arrive — both boundaries must re-stamp.
    app.sub_agents.get_mut("root/pi").unwrap().last_tool_at =
        Instant::now() - Duration::from_secs(120);

    app.handle_event(TuiEvent::Runtime(sub_tool_started("root/pi", "read_file")));
    let stamped = app.sub_agents.get("root/pi").unwrap().last_tool_at;
    assert!(
        stamped.elapsed() < Duration::from_secs(5),
        "start must re-stamp"
    );

    app.sub_agents.get_mut("root/pi").unwrap().last_tool_at =
        Instant::now() - Duration::from_secs(120);
    app.handle_event(TuiEvent::Runtime(sub_tool_finished(
        "root/pi",
        "read_file",
        "10 lines",
    )));
    let stamped = app.sub_agents.get("root/pi").unwrap().last_tool_at;
    assert!(
        stamped.elapsed() < Duration::from_secs(5),
        "finish must re-stamp"
    );
}
