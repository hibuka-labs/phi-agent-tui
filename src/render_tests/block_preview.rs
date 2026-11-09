//! Block rhythm and the preview ladder.

use super::super::*;
use crate::app::TuiEvent;
use crossterm::event::{KeyCode, KeyModifiers};
use phi_agent::{RuntimeEvent, SessionId};

// ── block rhythm + preview ladder (design §1 / §4) ──────────────────────────
// The layout may change; the content and its order may not. These pin the two
// rules that keep that true: spacers are a *rendering* decision that never
// enters history, and the ladder only ever decides how much of an intact
// payload to draw.

#[test]
fn block_spacers_are_render_time_only() {
    // A blank row goes *between* blocks (design §1). It must be a draw-time
    // decision — history, copy and session replay never acquire blank lines.
    let mut app = App::new();
    app.push_system("one");
    app.push_system("two");
    let before = app.transcript.output.len();
    let text = snapshot_text(&mut app, 80, 12);
    assert_eq!(
        app.transcript.output.len(),
        before,
        "rendering appended to history:\n{text}"
    );
    assert!(
        app.transcript
            .output
            .iter()
            .all(|l| !l.text.trim().is_empty()),
        "blank line in history: {:?}",
        app.transcript
            .output
            .iter()
            .map(|l| &l.text)
            .collect::<Vec<_>>()
    );

    let rows: Vec<&str> = text.lines().collect();
    assert_eq!(rows.first().map(|r| r.trim()), Some("one"), "\n{text}");
    assert_eq!(
        rows.get(1).map(|r| r.trim()),
        Some(""),
        "missing spacer between blocks:\n{text}"
    );
    assert_eq!(rows.get(2).map(|r| r.trim()), Some("two"), "\n{text}");
}

#[test]
fn call_and_its_folded_result_are_one_block() {
    // A tool block is the call *and* its results (design §1) — the blank row
    // goes between blocks, never inside one. A result that folds its payload is
    // still that call's result; the fold must not tear the block apart.
    let mut app = app_with_multi_row_result();
    for tier in [
        ResultTier::Compact,
        ResultTier::Default,
        ResultTier::Expanded,
    ] {
        app.result_tier = tier;
        let text = snapshot_text(&mut app, 80, 20);
        let rows: Vec<&str> = text.lines().collect();
        let call = rows
            .iter()
            .position(|r| r.contains("search_files"))
            .unwrap_or_else(|| panic!("call row missing at {tier:?}:\n{text}"));
        let result = rows
            .iter()
            .position(|r| r.trim_start().starts_with("<"))
            .unwrap_or_else(|| panic!("result row missing at {tier:?}:\n{text}"));
        assert!(
            result > call && result == call + 1,
            "call and result split apart at {tier:?} (call {call}, result {result}):\n{text}"
        );
    }
}

fn app_with_multi_row_result() -> App {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "search_files".to_string(),
        args_json: serde_json::json!({"pattern": "TODO"}).to_string(),
        agent_id: None,
        trace_id: None,
    }));
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: "search_files".to_string(),
        summary: "head line\nalpha\nbeta\ngamma".to_string(),
        denied: false,
        details: None,
        agent_id: None,
        trace_id: None,
    }));
    app
}

#[test]
fn tool_result_rides_the_preview_ladder() {
    // The tool's answer is stored whole and drawn 0 / 2 / all rows. Only the
    // amount on screen changes — never the text itself, never its order.
    let mut app = app_with_multi_row_result();

    // Compact: nothing of the payload, just the count of what is held back.
    assert_eq!(app.result_tier, ResultTier::Compact);
    let text = snapshot_text(&mut app, 80, 20);
    assert!(text.contains("  < head line"), "meta row missing:\n{text}");
    assert!(text.contains("... +3 lines"), "fold hint missing:\n{text}");
    assert!(
        !text.contains("alpha") && !text.contains("gamma"),
        "payload leaked while compact:\n{text}"
    );

    // Default: two rows, the rest counted.
    app.result_tier = ResultTier::Default;
    let text = snapshot_text(&mut app, 80, 20);
    assert!(text.contains("alpha") && text.contains("beta"), "\n{text}");
    assert!(!text.contains("gamma"), "tail shown at default:\n{text}");
    assert!(text.contains("... +1 lines"), "fold hint missing:\n{text}");

    // Expanded: the whole answer, no hint.
    app.result_tier = ResultTier::Expanded;
    let text = snapshot_text(&mut app, 80, 20);
    for row in ["alpha", "beta", "gamma"] {
        assert!(text.contains(row), "{row} missing when expanded:\n{text}");
    }
    assert!(
        !text.contains("... +"),
        "fold hint left behind when expanded:\n{text}"
    );

    // The ladder is display-only: the payload is still whole in history.
    let result = app
        .transcript
        .output
        .iter()
        .find(|l| l.kind == LineKind::ToolResult)
        .expect("result line present");
    match &result.detail {
        Some(phi_tui::lines::LineDetail::Folded { raw, .. }) => {
            assert_eq!(raw, "head line\nalpha\nbeta\ngamma");
        }
        other => panic!("result text must be kept whole, got {other:?}"),
    }
}

#[test]
fn ctrl_e_cycles_the_result_tier() {
    let mut app = App::new();
    assert_eq!(app.result_tier, ResultTier::Compact);
    app.handle_key(KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert_eq!(app.result_tier, ResultTier::Default);
    app.handle_key(KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert_eq!(app.result_tier, ResultTier::Expanded);
    app.handle_key(KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert_eq!(app.result_tier, ResultTier::Compact);
}

#[test]
fn tool_state_glyph_reflects_call_state() {
    // The `*` in `text` is a marker slot, not chrome: the renderer substitutes
    // the state glyph, so a settled call is legible without opening anything.
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "search_files".to_string(),
        args_json: serde_json::json!({"pattern": "TODO"}).to_string(),
        agent_id: None,
        trace_id: None,
    }));
    let text = snapshot_text(&mut app, 80, 20);
    let row = text
        .lines()
        .find(|r| r.contains("search_files"))
        .unwrap_or_else(|| panic!("tool row missing:\n{text}"));
    assert!(
        !row.trim_start().starts_with('*'),
        "marker slot leaked into display: {row:?}"
    );

    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: "search_files".to_string(),
        summary: "1 hit".to_string(),
        denied: false,
        details: None,
        agent_id: None,
        trace_id: None,
    }));
    let text = snapshot_text(&mut app, 80, 20);
    let row = text
        .lines()
        .find(|r| r.contains("search_files"))
        .unwrap_or_else(|| panic!("tool row missing:\n{text}"));
    assert!(
        row.trim_start().starts_with('o'),
        "done glyph missing: {row:?}"
    );

    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "execute_command".to_string(),
        args_json: serde_json::json!({"command": "rm -rf /"}).to_string(),
        agent_id: None,
        trace_id: None,
    }));
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: "execute_command".to_string(),
        summary: String::new(),
        denied: true,
        details: None,
        agent_id: None,
        trace_id: None,
    }));
    let text = snapshot_text(&mut app, 80, 20);
    let row = text
        .lines()
        .find(|r| r.contains("execute_command") && r.trim_start().starts_with('x'))
        .unwrap_or_else(|| panic!("denied glyph missing:\n{text}"));
    assert!(row.trim_start().starts_with('x'));
}

#[test]
fn edit_diff_never_folds_at_any_tier() {
    // Red line from the design review: an edit *is* the evidence of what
    // changed, so no tier may drop a single line of it. The ladder is for
    // artifacts (a written file's content), never for a transformation.
    let mut app = App::new();
    let args = serde_json::json!({
        "path": "src/main.rs",
        "edits": [
            {
                "old_text": "fn main() {\n    println!(\"hello\");\n}",
                "new_text": "fn main() {\n    println!(\"world\");\n}"
            }
        ]
    });
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "edit_file".to_string(),
        args_json: args.to_string(),
        agent_id: None,
        trace_id: None,
    }));

    for tier in [
        ResultTier::Compact,
        ResultTier::Default,
        ResultTier::Expanded,
    ] {
        app.result_tier = tier;
        let text = snapshot_text(&mut app, 100, 30);
        assert!(text.contains("┌─ src/main.rs"), "{tier:?}:\n{text}");
        assert!(
            text.contains("println") && text.contains("-") && text.contains("+"),
            "diff content dropped at {tier:?}:\n{text}"
        );
        assert!(
            !text.contains("... +"),
            "a diff must never be folded: {tier:?}:\n{text}"
        );
    }
}

#[test]
fn write_create_rides_the_ladder_but_keeps_the_content() {
    // A created file is an artifact: its body folds onto the ladder. The
    // content itself is never rewritten or lost — expanding shows all of it.
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

    assert_eq!(app.result_tier, ResultTier::Compact);
    let text = snapshot_text(&mut app, 80, 20);
    assert!(
        text.contains("... +3 lines"),
        "written content should be folded when compact:\n{text}"
    );

    app.result_tier = ResultTier::Expanded;
    let text = snapshot_text(&mut app, 80, 20);
    for row in ["fn hello()", "println", "}"] {
        assert!(text.contains(row), "{row} missing when expanded:\n{text}");
    }
}
