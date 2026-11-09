//! Plan block rendering.

use super::super::*;
use super::common::*;
use phi_agent::PlanStepStatus;
use phi_tui::lines::OutputLine;

#[test]
fn plan_renders_status_markers() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(plan(
        "目标",
        vec![
            ("已完成", PlanStepStatus::Completed),
            ("进行中", PlanStepStatus::InProgress),
            ("待办", PlanStepStatus::Pending),
        ],
    )));
    let lines: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert!(
        lines.iter().any(|l| l.contains("📋 目标")),
        "got: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("✅ 已完成")),
        "got: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("🔄 进行中")),
        "got: {lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("- 待办")), "got: {lines:?}");
}

/// protects: a plan update re-pastes the block at the flow tail — work that
/// landed after the old block stays above the fresh one, and the old rows
/// vanish with no residue and no blank row left at the old spot (批4 #11 AC1,
/// row-for-row). fails_when: the update patches the block in place (it stays
/// above the follow-up work) or leaves duplicated/partial rows behind.
/// no_claim: how the viewport follows the move — that seam is pinned in
/// `render_tests/plan_follow.rs`.
#[test]
fn plan_update_moves_the_block_to_the_tail() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(plan(
        "目标",
        vec![("步骤1", PlanStepStatus::Completed)],
    )));
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        detail: None,
        text: "follow-up work lands under the plan".to_string(),
        kind: LineKind::Normal,
        tool_state: None,
    });
    app.handle_event(TuiEvent::Runtime(plan(
        "目标",
        vec![
            ("步骤1", PlanStepStatus::Completed),
            ("步骤2", PlanStepStatus::InProgress),
        ],
    )));
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        texts,
        [
            "follow-up work lands under the plan",
            "📋 目标",
            "   ✅ 步骤1",
            "   🔄 步骤2",
        ],
        "old block gone, fresh block at the tail: {texts:?}"
    );
}

#[test]
fn plan_renders_explanation() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(plan_ex(
        "目标",
        Some("检测到已有配置，直接复用"),
        vec![("步骤", PlanStepStatus::Pending)],
    )));
    assert!(
        app.transcript
            .output
            .iter()
            .any(|l| l.text.contains("↳ 检测到已有配置"))
    );
}

#[test]
fn update_plan_tool_result_line_is_suppressed() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(tool_started("update_plan")));
    app.handle_event(TuiEvent::Runtime(tool_finished("update_plan", false)));
    assert!(
        !app.transcript
            .output
            .iter()
            .any(|l| l.text.contains("update_plan"))
    );
}

#[test]
fn plan_resets_across_turns() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(plan(
        "目标A",
        vec![("步骤", PlanStepStatus::Pending)],
    )));
    // Starting a new turn (submit) clears the tracked plan range, so the next
    // plan appends instead of replacing the previous turn's plan.
    assert_eq!(submit(&mut app, "next"), Action::Submit("next".to_string()));
    app.handle_event(TuiEvent::Runtime(plan(
        "目标B",
        vec![("步骤B", PlanStepStatus::Pending)],
    )));
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("📋 目标A")),
        "target A kept: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("📋 目标B")),
        "target B appended: {texts:?}"
    );
    assert_eq!(texts.iter().filter(|t| t.contains("📋")).count(), 2);
}
