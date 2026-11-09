//! CJK width-safety guard for chrome.

use super::super::*;
use crate::app::TuiEvent;
use phi_agent::{RuntimeEvent, SessionId};

// ── CJK width-safety guard (session 20260908_a9f7a846) ──────────────────────
// CJK terminal fonts render symbols like ⏺ ✓ ● → ⚠ … — as double-width while
// the layout counts them single-width; the accumulated drift wrapped rows and
// pushed the composer off-screen. All chrome now uses ASCII; these tests keep
// it that way.

/// Symbols whose rendered width disagrees with `unicode-width` in CJK fonts.
/// Box drawing (─│╭) and block elements (█) are deliberately NOT listed:
/// CJK mono fonts keep those single-width (proven in the session above).
const CJK_WIDTH_UNSAFE: &[char] = &[
    '⏺', '⏸', '⏳', '✓', '✔', '✗', '✘', '●', '○', '→', '←', '⇒', '⇐', '⟳', '⚠', '❯', '×', '÷', '±',
    '≤', '≥', '≠', '≈', '≡', '…', '·', '—', '–', '•', '√', '§', '∑', '∏', '∫', '∂', '∇', '∞', '①',
    '▸',
];

#[test]
fn rendered_chrome_stays_cjk_width_safe() {
    let mut app = App::new();
    // Drive the real event pipeline so production format strings are exercised.
    let args = serde_json::json!({"path": "src/main.rs"});
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "read_file".to_string(),
        args_json: args.to_string(),
        agent_id: None,
        trace_id: None,
    }));
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: "read_file".to_string(),
        summary: "10 lines".to_string(),
        denied: false,
        details: None,
        agent_id: None,
        trace_id: None,
    }));
    // Every status variant's chrome.
    let mut status_texts = Vec::new();
    status_texts.push(app.status_line());
    app.status = AgentStatus::Running {
        phase: Phase::Thinking,
    };
    status_texts.push(app.status_line());
    app.status = AgentStatus::Running {
        phase: Phase::Streaming,
    };
    status_texts.push(app.status_line());
    app.status = AgentStatus::Running {
        phase: Phase::ToolCall {
            tool: "edit_file".into(),
        },
    };
    status_texts.push(app.status_line());
    app.status = AgentStatus::Running {
        phase: Phase::AwaitingApproval,
    };
    status_texts.push(app.status_line());
    app.status = AgentStatus::Idle;
    status_texts.push(app.status_line());
    // Idle renders the tips band; its label/text are chrome too. The pool is
    // the product's (ShellSpec tips slot) — a local fixture stands in here.
    app.set_tips(vec![crate::tips::TipItem::tip(
        "Press Ctrl+Y to copy the last reply",
    )]);

    let text = snapshot_text(&mut app, 100, 30);
    let all = text + &status_texts.join("\n");
    let offenders: Vec<char> = all
        .chars()
        .filter(|c| CJK_WIDTH_UNSAFE.contains(c))
        .collect();
    assert!(
        offenders.is_empty(),
        "CJK-double-width glyphs in rendered chrome: {offenders:?}\n{all}"
    );
}

#[test]
fn chrome_sources_stay_cjk_width_safe() {
    // Scan the chrome-rendering sources (outside comments) for the symbols.
    // (The product's own chrome — e.g. its wordmark banner — carries its own
    // scan over its own paths.)
    const FILES: &[&str] = &[
        "src/render/mod.rs",
        "src/render/frame.rs",
        "src/render/output.rs",
        "src/render/faces.rs",
        "src/render/popups.rs",
        "src/render/lines.rs",
        "src/render/snapshot.rs",
        "src/app/mod.rs",
        "src/app/compose.rs",
        "src/app/event.rs",
        "src/app/interaction.rs",
        "src/run/mod.rs",
        "src/run/pump.rs",
        "src/run/input.rs",
        "src/run/agent_loop.rs",
        "src/run/resume_replay.rs",
        "src/run/notices.rs",
        "src/run/tests.rs",
        "src/task_panel.rs",
        "src/child_results.rs",
        "src/handlers/runtime/mod.rs",
        "src/handlers/runtime/tool_text.rs",
        "src/picker.rs",
    ];
    for file in FILES {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
        let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {file}: {e}"));
        let mut exempt = false;
        for (i, line) in source.lines().enumerate() {
            // Region markers live in comments (checked on the raw line): a
            // marked block recolours glyphs the product already wrote and
            // computes no columns, so it can't shift any layout.
            if line.contains("cjk-glyph-exempt: begin") {
                assert!(!exempt, "{file}:{} nested cjk-glyph-exempt begin", i + 1);
                exempt = true;
                continue;
            }
            if line.contains("cjk-glyph-exempt: end") {
                assert!(
                    exempt,
                    "{file}:{} cjk-glyph-exempt end without begin",
                    i + 1
                );
                exempt = false;
                continue;
            }
            if exempt {
                continue;
            }
            let code = line.split_once("//").map_or(line, |(code, _)| code);
            let offenders: Vec<char> = code
                .chars()
                .filter(|c| CJK_WIDTH_UNSAFE.contains(c))
                .collect();
            assert!(
                offenders.is_empty(),
                "{file}:{} renders CJK-double-width glyphs {offenders:?}: {line}",
                i + 1
            );
        }
        assert!(!exempt, "{file}: unclosed cjk-glyph-exempt region");
    }
}
