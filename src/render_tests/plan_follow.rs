//! Plan-block follow-tail (批4 #11): a plan update MOVES the block to the
//! flow tail, and the viewport must not be startled by the move — a
//! scrolled-up pane stays row-for-row identical (the anchor translates
//! through the removed rows), and a following view stays pinned to the
//! bottom where the re-pasted block is visible.

use super::super::*;
use crate::app::TuiEvent;
use phi_agent::{PlanItem, PlanStepStatus, RuntimeEvent, SessionId};
use phi_tui::lines::OutputLine;

fn filler(text: &str) -> OutputLine<ShellToken> {
    OutputLine {
        spans: None,
        original: None,
        detail: None,
        text: text.to_string(),
        kind: LineKind::Normal,
        tool_state: None,
    }
}

fn plan_event(objective: &str, steps: &[(&str, PlanStepStatus)]) -> TuiEvent {
    TuiEvent::Runtime(RuntimeEvent::PlanUpdated {
        session_id: SessionId::new(1),
        objective: objective.to_string(),
        explanation: None,
        plan: steps
            .iter()
            .map(|(step, status)| PlanItem {
                step: step.to_string(),
                status: *status,
            })
            .collect(),
        agent_id: None,
        trace_id: None,
    })
}

/// Mid-flow plan fixture: filler rows, a 3-row plan block near the top, then
/// enough filler that the window can scroll well below the block.
fn app_with_plan_mid_flow() -> App {
    let mut app = App::new();
    for i in 0..5 {
        app.transcript.push(filler(&format!("early row {i}")));
    }
    app.handle_event(plan_event(
        "目标A",
        &[
            ("步骤A1", PlanStepStatus::Completed),
            ("步骤A2", PlanStepStatus::Pending),
        ],
    ));
    for i in 0..60 {
        app.transcript.push(filler(&format!("filler row {i}")));
    }
    app
}

/// protects: scrolling mid-history and getting a plan update must not move a
/// single row the reader can see — the block's old rows vanish above the
/// window and the fresh block lands at the tail (off-screen), while the
/// scroll anchor translates through the removed rows (批4 #11 AC3).
/// fails_when: the pane shifts by the removed rows (the anchor resolved in
/// pre-splice numbering) or the view teleports to the tail. no_claim: the
/// status bar's scroll counter — it prints raw row numbers, which shift when
/// rows above the head vanish even though the visible content does not; and
/// what the plan rows say.
#[test]
fn scrolled_frame_holds_still_when_plan_repastes() {
    let mut app = app_with_plan_mid_flow();
    let _ = snapshot_text(&mut app, 80, 24); // establish viewport sizes
    assert!(app.scroll_up(), "fixture must be scrollable");
    let before = snapshot_text(&mut app, 80, 24);
    let head = before.lines().next().unwrap_or_default().to_string();
    assert!(
        head.contains("filler row"),
        "head must sit below the plan block:\n{before}"
    );
    let pane = app.viewport.viewport_height;

    // The re-paste: a 2-row block replaces the 3-row one — the flow shifts
    // under the reader by the difference.
    app.handle_event(plan_event("目标B", &[("步骤B1", PlanStepStatus::Pending)]));
    let after = snapshot_text(&mut app, 80, 24);
    // Row-for-row identical throughout the transcript pane (the top
    // `viewport_height` rows of the frame).
    let before_pane: Vec<&str> = before.lines().take(pane).collect();
    let after_pane: Vec<&str> = after.lines().take(pane).collect();
    assert_eq!(
        before_pane, after_pane,
        "the scrolled pane must be row-for-row identical:\n{after}"
    );

    // Positive control: the move really happened — the fresh block sits at
    // the bottom of the flow and the old one is gone from the top.
    app.scroll_to_bottom();
    let bottom = snapshot_text(&mut app, 80, 24);
    assert!(
        bottom.contains("📋 目标B"),
        "the re-pasted block must be at the tail:\n{bottom}"
    );
    app.viewport.scroll_to_top();
    let top = snapshot_text(&mut app, 80, 24);
    assert!(
        !top.contains("📋 目标A"),
        "the old block must leave no residue:\n{top}"
    );
}

/// protects: a plan update under a following view keeps the bottom pinned —
/// the re-pasted block is the point of the feature and must be visible at
/// the pane's foot, not floating above a frozen gap (the move nets rows out
/// here, exactly the case the tail-shrink hold would misfire on; AC4).
/// fails_when: the shrink engages the hold (blank rows under the new block,
/// `follow_bottom` lost) or the block lands above the window. no_claim: when
/// a real tail shrink should hold the head (that contract is
/// `set_visible_anchored`'s, pinned in phi-tui).
#[test]
fn follow_bottom_stays_pinned_when_plan_repastes() {
    let mut app = App::new();
    for i in 0..40 {
        app.transcript.push(filler(&format!("filler row {i}")));
    }
    app.handle_event(plan_event(
        "目标A",
        &[
            ("步骤A1", PlanStepStatus::Completed),
            ("步骤A2", PlanStepStatus::Completed),
            ("步骤A3", PlanStepStatus::Pending),
        ],
    ));
    let before = snapshot_text(&mut app, 80, 24);
    assert!(before.contains("📋 目标A"), "plan at the tail:\n{before}");

    // Shorter block in: 4 rows out, 2 rows in — a net shrink that must NOT
    // be mistaken for a tail collapse.
    app.handle_event(plan_event("目标B", &[("步骤B1", PlanStepStatus::Pending)]));
    let after = snapshot_text(&mut app, 80, 24);
    assert!(
        after.contains("📋 目标B") && after.contains("步骤B1"),
        "the new block must be visible:\n{after}"
    );
    assert!(!after.contains("目标A"), "the old block is gone:\n{after}");
    assert!(
        app.viewport.follow_bottom,
        "a move must not freeze a following view"
    );
    let total = app.visual_map.len();
    let height = app.viewport.viewport_height;
    let window = app.viewport.window_range(total, height);
    assert_eq!(
        window,
        (total - height)..total,
        "the window stays pinned to the bottom — no gap under the block"
    );
}
