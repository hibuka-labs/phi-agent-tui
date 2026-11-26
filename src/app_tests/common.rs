//! Shared fixtures for the App test modules.

use super::super::*;
use phi_agent::SessionId;
use phi_agent::{PlanItem, PlanStepStatus};

pub(super) fn text(s: &str) -> RuntimeEvent {
    RuntimeEvent::TextDelta {
        session_id: SessionId::new(1),
        text: s.to_string(),
        agent_id: None,
        trace_id: None,
    }
}

pub(super) fn thought(s: &str) -> RuntimeEvent {
    RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: s.to_string(),
        agent_id: None,
        trace_id: None,
    }
}

pub(super) fn tool_started(name: &str) -> RuntimeEvent {
    RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: name.to_string(),
        args_json: "{}".to_string(),
        agent_id: None,
        trace_id: None,
    }
}

pub(super) fn tool_started_with(name: &str, args_json: &str) -> RuntimeEvent {
    RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: name.to_string(),
        args_json: args_json.to_string(),
        agent_id: None,
        trace_id: None,
    }
}

pub(super) fn tool_finished(name: &str, denied: bool) -> RuntimeEvent {
    RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: name.to_string(),
        summary: "done".to_string(),
        agent_id: None,
        trace_id: None,
        denied,
        details: None,
    }
}

/// A finish whose summary is the tool's own answer (envelope, JSON, …) —
/// the result row projects that head.
pub(super) fn tool_finished_with_summary(name: &str, summary: &str) -> RuntimeEvent {
    RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: name.to_string(),
        summary: summary.to_string(),
        agent_id: None,
        trace_id: None,
        denied: false,
        details: None,
    }
}

pub(super) fn run_finished(agent_id: Option<&str>) -> RuntimeEvent {
    RuntimeEvent::RunFinished {
        session_id: SessionId::new(1),
        agent_id: agent_id.map(String::from),
        trace_id: None,
    }
}

pub(super) fn run_cancelled(agent_id: Option<&str>) -> RuntimeEvent {
    RuntimeEvent::RunCancelled {
        session_id: SessionId::new(1),
        agent_id: agent_id.map(String::from),
        trace_id: None,
    }
}

pub(super) fn child_text(agent: &str, s: &str) -> RuntimeEvent {
    RuntimeEvent::TextDelta {
        session_id: SessionId::new(1),
        text: s.to_string(),
        agent_id: Some(agent.to_string()),
        trace_id: None,
    }
}

pub(super) fn child_thought(agent: &str, s: &str) -> RuntimeEvent {
    RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: s.to_string(),
        agent_id: Some(agent.to_string()),
        trace_id: None,
    }
}

pub(super) fn child_tool_started(agent: &str, name: &str) -> RuntimeEvent {
    RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: name.to_string(),
        args_json: "{}".to_string(),
        agent_id: Some(agent.to_string()),
        trace_id: None,
    }
}

pub(super) fn child_tool_finished(agent: &str, name: &str) -> RuntimeEvent {
    RuntimeEvent::ActionFinished {
        session_id: SessionId::new(1),
        tool_name: name.to_string(),
        summary: "done".to_string(),
        agent_id: Some(agent.to_string()),
        trace_id: None,
        denied: false,
        details: None,
    }
}

pub(super) fn plan(objective: &str, steps: Vec<(&str, PlanStepStatus)>) -> RuntimeEvent {
    plan_ex(objective, None, steps)
}

pub(super) fn plan_ex(
    objective: &str,
    explanation: Option<&str>,
    steps: Vec<(&str, PlanStepStatus)>,
) -> RuntimeEvent {
    RuntimeEvent::PlanUpdated {
        session_id: SessionId::new(1),
        objective: objective.to_string(),
        explanation: explanation.map(String::from),
        plan: steps
            .into_iter()
            .map(|(step, status)| PlanItem {
                step: step.to_string(),
                status,
            })
            .collect(),
        agent_id: None,
        trace_id: None,
    }
}

/// Simulates the run.rs send path: key handling yields the action, and the
/// loop commits turn state only when it actually queues a `Cmd::Run`. A submit
/// intercepted before that point (/resume, /upgrade) never calls
/// [`App::begin_user_turn`] — see `intercepted_submit_commits_no_turn_state`.
pub(super) fn submit(app: &mut App, input: &str) -> Action {
    app.composer.clear();
    app.composer.insert_str(input);
    let action = app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
    if let Action::Submit(text) = &action {
        app.begin_user_turn(text);
    }
    action
}

/// Like [`submit`], but through the cancel-rescue arm ([`App::begin_rescuable_turn`])
/// — the root send path in run/input.rs.
pub(super) fn submit_rescuable(app: &mut App, input: &str) {
    app.composer.clear();
    app.composer.insert_str(input);
    let action = app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
    if let Action::Submit(text) = &action {
        app.begin_rescuable_turn(text);
    }
}

pub(super) fn pending_approval(app: &mut App) {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    app.approval_queue.push_back(ApprovalItem {
        request: ApprovalRequest {
            title: "write_file".to_string(),
            message: "Write file: src/lib.rs".to_string(),
            action_key: None,
            risk_level: phi_agent::RiskLevel::Sensitive,
            raw: None,
            source: None,
        },
        decision_tx: tx,
    });
}
