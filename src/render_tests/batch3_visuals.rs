//! Batch 3 visual rules (#3 / #4 / #13) at the render layer.
//!
//! These assert what the user sees, not the numbers behind it: the task
//! frame draws one rule instead of a box, the process row splits its live
//! phase word from the parenthetical hint, and the name column carries a
//! wide name whole.

use super::super::*;
use crate::app::{App, SubAgentState, SubAgentStatus};

/// A panel with one live sub-agent, so the frame has a row to show.
fn panel_app() -> App {
    let mut app = App::new();
    app.sub_agents.insert(
        "root/analyze".to_string(),
        SubAgentState {
            name: "analyze".to_string(),
            status: SubAgentStatus::Running,
            files: Vec::new(),
            started_at: std::time::Instant::now(),
            completed_at: None,
            last_tool_at: std::time::Instant::now(),
            events: Vec::new(),
        },
    );
    app.task_panel.frame_open = true;
    app
}

#[test]
fn the_task_frame_draws_a_top_rule_and_no_box() {
    // #3 任务面板框只留上边框: the frame keeps its top rule and drops the
    // sides and the bottom. A box would show rounded corners — none may
    // survive. (The rows' own `│` column separators are content, not the
    // frame's border, so only the box-drawing glyphs are checked.)
    let mut app = panel_app();
    let snap = crate::render::snapshot_text(&mut app, 80, 24);
    for glyph in ['╭', '╰', '╮', '╯', '┌', '└', '┐', '┘'] {
        assert!(
            !snap.contains(glyph),
            "the frame must not draw a box ({glyph}):\n{snap}"
        );
    }
    // The rule itself is still there, with the title on it.
    let rule = snap
        .lines()
        .find(|l| l.contains("Tasks (1)"))
        .unwrap_or_else(|| panic!("the title must ride the top rule:\n{snap}"));
    assert!(rule.contains('─'), "the top rule is missing:\n{snap}");
    // The rows sit under the rule, with no left edge of their own: a box
    // would draw a `│` in the frame's first column. (The row's own `│`
    // column separators are content, not the frame's border.)
    let row_line = snap
        .lines()
        .position(|l| l.contains("analyze"))
        .expect("a task row");
    let rows: Vec<&str> = snap.lines().collect();
    let row = rows[row_line];
    assert!(
        !row.starts_with('│'),
        "the frame must not draw a left border:\n{snap}"
    );
}

#[test]
fn the_frame_close_hint_still_rides_the_top_rule() {
    let mut app = panel_app();
    let snap = crate::render::snapshot_text(&mut app, 80, 24);
    assert!(
        snap.contains("^T close"),
        "the close hint must stay on the rule:\n{snap}"
    );
}

#[test]
fn a_wide_task_name_is_shown_whole() {
    // #13 任务列宽: the name column widened to 28, so a 25-column name fits
    // without an ellipsis.
    let mut app = App::new();
    app.sub_agents.insert(
        "root/a".to_string(),
        SubAgentState {
            name: "analyze-deepseek-harness".to_string(),
            status: SubAgentStatus::Running,
            files: Vec::new(),
            started_at: std::time::Instant::now(),
            completed_at: None,
            last_tool_at: std::time::Instant::now(),
            events: Vec::new(),
        },
    );
    app.task_panel.frame_open = true;
    let snap = crate::render::snapshot_text(&mut app, 100, 24);
    assert!(
        snap.contains("analyze-deepseek-harness"),
        "the wide name must fit whole:\n{snap}"
    );
    assert!(
        !snap.contains("analyze-deeps..."),
        "the name must not be ellipsized at the old cap:\n{snap}"
    );
}

#[test]
fn the_process_row_splits_the_phase_word_from_its_hint() {
    // #4 配色分层: the live phase word and the parenthetical hint beside it
    // wear different slots. The split point is what the renderer uses, so
    // assert it survives into the rendered row.
    let mut app = App::new();
    app.status = AgentStatus::Running {
        phase: Phase::Thinking,
    };
    let snap = crate::render::snapshot_text(&mut app, 80, 24);
    assert!(
        snap.contains("thinking..."),
        "the phase word must be on the row:\n{snap}"
    );
    assert!(
        snap.contains("(Ctrl+C cancel)"),
        "the parenthetical hint must be on the row:\n{snap}"
    );
    // The hint is a separate span, so it is styled apart from the phase word.
    let runs = crate::render::snapshot_process_line_colors(&mut app, 80, 24);
    let thinking = runs
        .iter()
        .find(|(t, _)| t.contains("thinking..."))
        .expect("the phase word run");
    let hint = runs
        .iter()
        .find(|(t, _)| t.contains("(Ctrl+C cancel)"))
        .expect("the hint run");
    assert_ne!(
        thinking.1, hint.1,
        "the phase word and its hint must differ in color: {thinking:?} vs {hint:?}"
    );
}

#[test]
fn the_phase_word_takes_the_brands_bright_warm() {
    let mut app = App::new();
    app.status = AgentStatus::Running {
        phase: Phase::Thinking,
    };
    let runs = crate::render::snapshot_process_line_colors(&mut app, 80, 24);
    let thinking = runs
        .iter()
        .find(|(t, _)| t.contains("thinking..."))
        .expect("the phase word run");
    assert_eq!(
        thinking.1,
        app.theme().warm_bright,
        "the live phase word wears the brand's bright warm"
    );
}

#[test]
fn the_parenthetical_hint_wears_the_gray_hint_slot() {
    let mut app = App::new();
    app.status = AgentStatus::Running {
        phase: Phase::Thinking,
    };
    let runs = crate::render::snapshot_process_line_colors(&mut app, 80, 24);
    let hint = runs
        .iter()
        .find(|(t, _)| t.contains("(Ctrl+C cancel)"))
        .expect("the hint run");
    assert_eq!(
        hint.1,
        app.theme().hint,
        "the parenthetical hint drops to the gray hint slot"
    );
}
