//! Shared fixtures for the render test modules.

use super::super::*;
use phi_tui::lines::OutputLine;

pub(super) fn populated_app() -> App {
    let mut app = App::new();
    app.push_system("phimint — welcome");
    for i in 0..60 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            detail: None,
            text: format!("streamed line {i}"),
            kind: LineKind::Normal,
            tool_state: None,
        });
    }
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        detail: None,
        text: "* [sub/1] read_file {\"path\":\"src/lib.rs\"}".into(),
        kind: LineKind::Tool,
        tool_state: None,
    });
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        detail: None,
        text: "  ⛔ execute_command denied".into(),
        kind: LineKind::Error,
        tool_state: None,
    });
    app.composer.insert_str("hello\nworld");
    app.running = true;
    app.status = AgentStatus::Running {
        phase: Phase::ToolCall {
            tool: "build".into(),
        },
    };
    app
}
