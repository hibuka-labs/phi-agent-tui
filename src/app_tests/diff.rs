//! Diff detail rendering for file tools.

use super::super::*;
use super::common::*;
use phi_agent::SessionId;
use phi_tui::lines::{DiffLineKind, LineDetail, ToolState};

// ── Diff feature tests ──

#[test]
fn edit_file_produces_diff_detail() {
    let mut app = App::new();
    let args = serde_json::json!({
        "path": "src/main.rs",
        "edits": [
            { "old_text": "fn main() {\n    println!(\"hello\");\n}", "new_text": "fn main() {\n    println!(\"world\");\n}" }
        ]
    });
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "edit_file".to_string(),
        args_json: args.to_string(),
        agent_id: None,
        trace_id: None,
    }));

    // The tool line should have a Diff detail attached
    let tool_line = app.transcript.output.last().expect("tool line present");
    assert_eq!(tool_line.kind, LineKind::Tool);
    match &tool_line.detail {
        Some(LineDetail::Diff { path, hunks }) => {
            assert_eq!(path, "src/main.rs");
            assert!(!hunks.is_empty(), "should have diff hunks");
            // Should have a Del line and an Add line
            let all_lines: Vec<_> = hunks.iter().flat_map(|h| h.lines.iter()).collect();
            assert!(all_lines.iter().any(|l| l.kind == DiffLineKind::Del));
            assert!(all_lines.iter().any(|l| l.kind == DiffLineKind::Add));
        }
        other => panic!("expected LineDetail::Diff, got {other:?}"),
    }
}

#[test]
fn write_file_create_folds_the_written_content() {
    // A create is an artifact, not a transformation: the content rides the
    // preview ladder as a folded block. It is NOT an all-Add diff — that
    // rendering made a 10k-line write a 10k-line wall of green `+`.
    let mut app = App::new();
    let args = serde_json::json!({
        "path": "src/new.rs",
        "content": "fn hello() {\n    println!(\"hi\");\n}\n"
    });
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "write_file".to_string(),
        args_json: args.to_string(),
        agent_id: None,
        trace_id: None,
    }));

    let tool_line = app.transcript.output.last().expect("tool line present");
    match &tool_line.detail {
        Some(LineDetail::Folded {
            raw, line_count, ..
        }) => {
            assert_eq!(raw, "fn hello() {\n    println!(\"hi\");\n}\n");
            assert_eq!(*line_count, 3);
        }
        other => panic!("expected LineDetail::Folded, got {other:?}"),
    }
}

#[test]
fn write_file_overwrite_becomes_a_real_diff() {
    // An overwrite *is* a transformation, so once the tool reports the old
    // content the staged written-content block is swapped for an old→new diff —
    // the same always-full evidence treatment an edit_file gets.
    let mut app = App::new();
    let args = serde_json::json!({
        "path": "src/new.rs",
        "content": "fn hello() {\n    println!(\"bye\");\n}\n",
        "overwrite": true
    });
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "write_file".to_string(),
        args_json: args.to_string(),
        agent_id: None,
        trace_id: None,
    }));
    // Pre-swap: folded content.
    assert!(matches!(
        app.transcript.output.last().unwrap().detail,
        Some(LineDetail::Folded { .. })
    ));

    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: "write_file".to_string(),
        summary: "Updated file: src/new.rs".to_string(),
        agent_id: None,
        trace_id: None,
        denied: false,
        details: Some(serde_json::json!({
            "write_mode": "overwrite",
            "old_content": "fn hello() {\n    println!(\"hi\");\n}\n",
        })),
    }));

    let tool_line = app
        .transcript
        .output
        .iter()
        .find(|l| l.kind == LineKind::Tool)
        .expect("tool line present");
    match &tool_line.detail {
        Some(LineDetail::Diff { hunks, .. }) => {
            let all: Vec<_> = hunks.iter().flat_map(|h| h.lines.iter()).collect();
            assert!(all.iter().any(|l| l.kind == DiffLineKind::Del));
            assert!(all.iter().any(|l| l.kind == DiffLineKind::Add));
        }
        other => panic!("expected LineDetail::Diff after overwrite, got {other:?}"),
    }
    // Settled in place — the marker slot is redrawn from this.
    assert_eq!(tool_line.tool_state, Some(ToolState::Done));
}

#[test]
fn non_file_tool_has_no_detail() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    let tool_line = app.transcript.output.last().expect("tool line present");
    assert!(
        tool_line.detail.is_none(),
        "non-file tools should have no detail"
    );
}

#[test]
fn edit_file_diff_lines_appear_in_transcript() {
    let mut app = App::new();
    let args = serde_json::json!({
        "path": "src/lib.rs",
        "edits": [
            { "old_text": "old_fn()", "new_text": "new_fn()" }
        ]
    });
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "edit_file".to_string(),
        args_json: args.to_string(),
        agent_id: None,
        trace_id: None,
    }));

    // The tool line itself should be present
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert!(texts[0].contains("edit_file"));
}
