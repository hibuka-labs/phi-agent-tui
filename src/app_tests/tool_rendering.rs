//! Tool-call drafts, spawn/task result labels, and usage accounting.

use super::super::*;
use super::common::*;
use crate::theme::ShellToken;
use phi_agent::SessionId;
use phi_tui::lines::{LineDetail, MetaHead};

fn draft(index: usize, name: &str, args_len: usize, count: usize) -> RuntimeEvent {
    RuntimeEvent::ActionDraft {
        session_id: SessionId::new(1),
        index,
        name: name.to_string(),
        args_len,
        count,
        agent_id: None,
        trace_id: None,
    }
}

#[test]
fn tool_call_drafts_narrate_writing_and_clear_when_calls_materialize() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(draft(0, "spawn_agent", 881, 1)));
    assert!(app.is_active(), "a draft is a busy signal");
    let line = app.status_line();
    assert!(
        line.contains("writing spawn_agent (881 chars)..."),
        "got: {line}"
    );

    // A second parallel draft → plural form, summed chars.
    app.handle_event(TuiEvent::Runtime(draft(1, "spawn_agent", 400, 2)));
    let line = app.status_line();
    assert!(
        line.contains("writing 2 tool calls (1281 chars)..."),
        "got: {line}"
    );

    // The calls materializing (stream drained) ends the drafting window.
    app.handle_event(TuiEvent::Runtime(tool_started("spawn_agent")));
    assert!(
        app.tool_drafts.is_empty(),
        "materialized calls clear drafts"
    );
    let line = app.status_line();
    assert!(!line.contains("writing "), "got: {line}");
}

#[test]
fn tool_call_drafts_clear_on_turn_end() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(draft(0, "spawn_agent", 12, 1)));
    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    assert!(
        app.tool_drafts.is_empty(),
        "a finished turn can draft no more"
    );
}

#[test]
fn child_tool_drafts_never_touch_root_state() {
    let mut app = App::new();
    // A child's draft belongs to the task panel's liveness, not the root
    // status strip — and must not flip the root to Running.
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionDraft {
        session_id: SessionId::new(1),
        index: 0,
        name: "read_file".to_string(),
        args_len: 64,
        count: 1,
        agent_id: Some("root/child".to_string()),
        trace_id: None,
    }));
    assert!(app.tool_drafts.is_empty());
    assert_eq!(app.status, AgentStatus::Idle);

    // A root draft survives a child's call materializing (the root's calls
    // materialize together only after the root's own stream drains).
    app.handle_event(TuiEvent::Runtime(draft(0, "spawn_agent", 32, 1)));
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "read_file".to_string(),
        args_json: "{}".to_string(),
        agent_id: Some("root/child".to_string()),
        trace_id: None,
    }));
    assert_eq!(
        app.tool_drafts.len(),
        1,
        "child materialize must not clear root drafts"
    );
}

#[test]
fn spawn_task_brief_rides_the_preview_ladder() {
    let mut app = App::new();
    // Short first line → the meta row shows it whole; the fold's body owns
    // rows[1..], and the full brief is whole on the ladder.
    let task = "看一眼 pi 的 agent 循环\n然后还要看工具系统\n报告带文件行号";
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "spawn_agent".to_string(),
        args_json: serde_json::json!({"task_name": "analyze-pi", "task": task}).to_string(),
        agent_id: None,
        trace_id: None,
    }));
    let last = app.transcript.output.last().unwrap();
    assert!(
        last.text.starts_with("* spawn_agent analyze-pi - "),
        "got: {}",
        last.text
    );
    match &last.detail {
        Some(LineDetail::Folded { raw, meta_head, .. }) => {
            assert_eq!(raw, task, "the full brief rides the ladder, whole");
            assert_eq!(*meta_head, MetaHead::Whole);
        }
        other => panic!("expected Folded brief, got {other:?}"),
    }
}

#[test]
fn spawn_task_head_is_abbreviated_when_the_first_line_is_long() {
    let mut app = App::new();
    // A first line past the gist budget is truncated in the meta row; the
    // ladder restores the full head at full expansion (Abbreviated), so the
    // truncation never loses text.
    let head = "分析".repeat(40); // 160 display cols > the 56-col gist
    let task = format!("{head}\n第二行");
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "spawn_agent".to_string(),
        args_json: serde_json::json!({"task_name": "analyze-pi", "task": task}).to_string(),
        agent_id: None,
        trace_id: None,
    }));
    let last = app.transcript.output.last().unwrap();
    assert!(
        !last.text.contains(&head),
        "meta row must not dump the head, got: {}",
        last.text
    );
    match &last.detail {
        Some(LineDetail::Folded { meta_head, .. }) => {
            assert_eq!(*meta_head, MetaHead::Abbreviated);
        }
        other => panic!("expected Folded brief, got {other:?}"),
    }
}

#[test]
fn spawn_result_is_humanized_with_raw_preserved() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "spawn_agent".to_string(),
        args_json: serde_json::json!({"task_name": "analyze-pi", "task": "看看 pi"}).to_string(),
        agent_id: None,
        trace_id: None,
    }));
    let summary = serde_json::json!({
        "agent_path": "root/analyze-pi",
        "message": "Agent spawned successfully (tools: read_only; registered: read_file, grep)"
    })
    .to_string();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: "spawn_agent".to_string(),
        summary: summary.clone(),
        agent_id: None,
        trace_id: None,
        denied: false,
        details: None,
    }));
    let last = app.transcript.output.last().unwrap();
    assert_eq!(last.text, "  < spawned root/analyze-pi (read_only)");
    match &last.detail {
        Some(LineDetail::Folded { raw, meta_head, .. }) => {
            assert_eq!(
                raw, &summary,
                "summary is display-only — raw rides the fold, never rewritten"
            );
            assert_eq!(
                *meta_head,
                MetaHead::None,
                "the label is a fact, not the payload's head"
            );
        }
        other => panic!("expected Folded result, got {other:?}"),
    }
}

#[test]
fn spawn_result_label_keeps_recycled_and_degraded_facts() {
    let mut app = App::new();
    let spawn = |app: &mut App| {
        app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
            session_id: SessionId::new(1),
            tool_name: "spawn_agent".to_string(),
            args_json: serde_json::json!({"task_name": "t", "task": "x"}).to_string(),
            agent_id: None,
            trace_id: None,
        }));
    };
    let finish = |app: &mut App, message: &str| {
        app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionFinished {
            session_id: SessionId::new(1),
            tool_name: "spawn_agent".to_string(),
            summary: serde_json::json!({"agent_path": "root/t", "message": message}).to_string(),
            agent_id: None,
            trace_id: None,
            denied: false,
            details: None,
        }));
        app.transcript.output.last().unwrap().text.clone()
    };

    spawn(&mut app);
    let text = finish(
        &mut app,
        "Agent spawned successfully (recycled a finished agent with the same path)",
    );
    assert_eq!(text, "  < spawned root/t (recycled)", "got: {text}");

    spawn(&mut app);
    let text = finish(
        &mut app,
        "Agent spawned successfully (tools degraded to read-only: sandbox policy)",
    );
    assert_eq!(
        text, "  < spawned root/t (degraded to read-only)",
        "got: {text}"
    );
}

#[test]
fn background_start_result_is_humanized_with_raw_preserved() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "execute_command",
        r#"{"background":true,"command":"vite"}"#,
    )));
    let summary = serde_json::json!({
        "background": true,
        "task_id": "bg_ab12cd34",
        "message": "Command started in background. Use task_output to check."
    })
    .to_string();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: "execute_command".to_string(),
        summary: summary.clone(),
        agent_id: None,
        trace_id: None,
        denied: false,
        details: None,
    }));
    let last = app.transcript.output.last().unwrap();
    assert_eq!(last.text, "  < background task bg_ab12cd34 started");
    match &last.detail {
        Some(LineDetail::Folded { raw, meta_head, .. }) => {
            assert_eq!(
                raw, &summary,
                "summary is display-only — raw rides the fold, never rewritten"
            );
            assert_eq!(
                *meta_head,
                MetaHead::None,
                "the label is a fact, not the payload's head"
            );
        }
        other => panic!("expected Folded result, got {other:?}"),
    }
}

#[test]
fn write_file_content_head_is_never_restored() {
    let mut app = App::new();
    // A long first line wraps at display time; `MetaHead::None` is what keeps
    // the ladder from re-drawing that head on top of itself at full expansion
    // — the meta row is `* write_file <path>`, a label, never the content.
    let content = "fn main() { /* a deliberately long first line that wraps on any terminal and must not be drawn twice when the fold expands */ }\nsecond line\n";
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "write_file".to_string(),
        args_json: serde_json::json!({"path": "/tmp/x.rs", "content": content}).to_string(),
        agent_id: None,
        trace_id: None,
    }));
    let last = app.transcript.output.last().unwrap();
    match &last.detail {
        Some(LineDetail::Folded { raw, meta_head, .. }) => {
            assert_eq!(raw, content);
            assert_eq!(*meta_head, MetaHead::None);
        }
        other => panic!("expected Folded content, got {other:?}"),
    }
}

#[test]
fn activity_suffix_formats_minutes_and_tokens() {
    let mut app = App::new();
    app.activity_since = Some(Instant::now() - std::time::Duration::from_secs(91));
    assert_eq!(app.activity_suffix(), " - 1m32s");
    // The window (provider-measured) rides along once any usage arrives —
    // not the session burn.
    app.window_tokens = 87_300;
    assert_eq!(app.activity_suffix(), " - 1m32s - 87.3k tokens");
}

#[test]
fn usage_events_feed_status_and_turn_summary_tokens() {
    let mut app = App::new();
    app.running = true;
    app.status = AgentStatus::Running {
        phase: Phase::Thinking,
    };
    app.begin_activity();
    // OpenAI shape: `prompt_tokens` includes the cached slice and the
    // provider reports a total — the total is the window, no double count.
    app.handle_event(TuiEvent::Usage {
        input_tokens: 12_000,
        output_tokens: 300,
        cache_read_tokens: Some(9_000),
        total_tokens: Some(12_300),
    });
    let line = app.status_line();
    assert!(
        line.contains(" - 12.3k tokens"),
        "status shows the provider's window total, got: {line}"
    );

    // Anthropic shape: no total; the fields are disjoint and sum exactly.
    app.handle_event(TuiEvent::Usage {
        input_tokens: 500,
        output_tokens: 2_200,
        cache_read_tokens: Some(9_800),
        total_tokens: None,
    });
    let line = app.status_line();
    assert!(
        line.contains(" - 12.5k tokens"),
        "status shows the summed window when no total is reported, got: {line}"
    );

    app.handle_event(TuiEvent::TurnDone);
    let last = app.transcript.output.last().expect("turn-final line");
    assert!(
        last.text.starts_with("crunched for ") && last.text.contains(" - 15.0k tokens - done"),
        "turn-final line reports the stretch's tokens, got: {}",
        last.text
    );

    // The next stretch restarts the turn tally; the window stays the last
    // measurement until the next model call refreshes it.
    app.running = true;
    app.status = AgentStatus::Running {
        phase: Phase::Thinking,
    };
    app.begin_activity();
    app.handle_event(TuiEvent::Usage {
        input_tokens: 400,
        output_tokens: 100,
        cache_read_tokens: None,
        total_tokens: None,
    });
    let line = app.status_line();
    assert!(
        line.contains(" - 500 tokens"),
        "status shows the new call's window, got: {line}"
    );
    app.handle_event(TuiEvent::TurnDone);
    let last = app.transcript.output.last().expect("turn-final line");
    assert!(
        last.text.contains(" - 500 tokens - done"),
        "turn-final line reports only the new stretch, got: {}",
        last.text
    );
}

#[test]
fn turn_verdict_rows_wear_the_quiet_chrome_face() {
    // The `crunched …` row is a settled fact, not conversation: the whole row
    // carries one quiet-chrome run (`ShellToken::Faint`) so it paints at the
    // theme's grey, and a failed turn's marker takes the subdued error slot.
    // fails_when: the verdict row loses its run (renders at the System base
    // again) or the error verdict goes grey.
    let mut app = App::new();
    app.running = true;
    app.status = AgentStatus::Running {
        phase: Phase::Thinking,
    };
    app.begin_activity();
    app.handle_event(TuiEvent::TurnDone);
    let row = app.transcript.output.last().expect("verdict row");
    assert!(row.text.starts_with("crunched for "), "{}", row.text);
    let spans = row
        .spans
        .as_deref()
        .expect("verdict row carries its style run");
    assert_eq!(spans.len(), 1, "one full-row run, got {spans:?}");
    assert_eq!(spans[0].start, 0);
    assert_eq!(spans[0].len, row.text.len());
    assert_eq!(spans[0].style, ShellToken::Faint);

    // The failure marker keeps the subdued error slot, same full-row shape.
    let mut app = App::new();
    app.running = true;
    app.status = AgentStatus::Running {
        phase: Phase::Thinking,
    };
    app.begin_activity();
    app.handle_event(TuiEvent::TurnError("boom".into()));
    let row = app.transcript.output.last().expect("error verdict row");
    assert!(row.text.starts_with("crunched for "), "{}", row.text);
    let spans = row.spans.as_deref().expect("style run");
    assert_eq!(spans.len(), 1, "one full-row run, got {spans:?}");
    assert_eq!(spans[0].style, ShellToken::Error);
}
