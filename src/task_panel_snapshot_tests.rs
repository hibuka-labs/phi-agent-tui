//! Phase-5 lifecycle-snapshot reconciliation: rows appear at spawn, flip on
//! registry facts (`done`/`failed`/`cancelled`/`closed`), reopen on re-task,
//! and leave at the death sweeps (Rev 2026-11-04, death table 死期表 — a store entry is
//! not a review window, the ledger line is).

use crate::app::*;
use crate::app::{App, FocusTarget, SubAgentStatus};
use phi_agent::{AgentSnapshot, RegistrySnapshot};

// ── Phase 5: lifecycle snapshot reconciliation ───────────────────────────────

/// A snapshot with a single agent in the given derived status.
fn snap(path: &str, status: &str) -> std::sync::Arc<RegistrySnapshot> {
    std::sync::Arc::new(RegistrySnapshot {
        agents: vec![AgentSnapshot {
            path: path.to_string(),
            status: status.to_string(),
            running_secs: None,
            last_activity_secs: None,
            tool_calls: 0,
            task: None,
            pending_results: 0,
            last_error: None,
        }],
    })
}

#[test]
fn snapshot_creates_entry_at_spawn_before_first_tool_event() {
    // Phase 5's whole point: the event-driven path only discovered a child
    // at its first ActionStarted; the snapshot makes the panel entry
    // appear the moment the agent registers.
    let mut app = App::new();
    app.handle_event(TuiEvent::Lifecycle(snap("root/worker", "running")));

    let state = app.sub_agents.get("root/worker").expect("entry exists");
    assert_eq!(state.status, SubAgentStatus::Running);
    assert!(
        app.transcript
            .output
            .iter()
            .any(|l| l.text.contains("[root/worker] started"))
    );
}

#[test]
fn snapshot_running_to_done_sets_completed_at() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Lifecycle(snap("root/worker", "running")));
    // Pin (inspection freeze 检视冻结): the done fact triggers the all-complete sweep and we want to read
    // the flipped entry afterwards.
    app.task_panel.focus = FocusTarget::TaskList("root/worker".to_string());
    app.handle_event(TuiEvent::Lifecycle(snap("root/worker", "done")));

    let state = app.sub_agents.get("root/worker").unwrap();
    assert_eq!(state.status, SubAgentStatus::Done);
    assert!(state.completed_at.is_some());
}

#[test]
fn snapshot_done_to_running_reopens_retasked_entry() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Lifecycle(snap("root/worker", "running")));
    // Pin (inspection freeze 检视冻结): the done fact must not sweep the entry before the
    // re-task — the reopen path is what is under test, not a re-insert.
    app.task_panel.focus = FocusTarget::TaskList("root/worker".to_string());
    app.handle_event(TuiEvent::Lifecycle(snap("root/worker", "done")));
    // send_message(trigger=true) re-tasks the agent → fact flips back.
    app.handle_event(TuiEvent::Lifecycle(snap("root/worker", "running")));

    let state = app.sub_agents.get("root/worker").unwrap();
    assert_eq!(state.status, SubAgentStatus::Running);
    assert!(
        state.completed_at.is_none(),
        "reopened entry must not be reaped"
    );
}

#[test]
fn snapshot_unknown_done_agent_is_not_inserted() {
    // A freshly registered agent reads `done` until its first send_task —
    // inserting it would show a phantom done entry.
    let mut app = App::new();
    app.handle_event(TuiEvent::Lifecycle(snap("root/fresh", "done")));

    assert!(app.sub_agents.is_empty());
}

#[test]
fn snapshot_disappearance_marks_done() {
    // Unregistration = normal exit after posting, or close. The terminal
    // fact is recorded — and with nothing else running the all-complete sweep
    // takes the row at once (Rev 2026-11-04: the store is not a review
    // window, the ledger line is).
    let mut app = App::new();
    app.handle_event(TuiEvent::Lifecycle(snap("root/worker", "running")));
    app.handle_event(TuiEvent::Lifecycle(std::sync::Arc::new(RegistrySnapshot {
        agents: vec![],
    })));

    assert!(
        !app.sub_agents.contains_key("root/worker"),
        "all-complete sweep"
    );
    assert!(
        app.transcript
            .output
            .iter()
            .any(|l| l.text.contains("[root/worker] done")),
        "the ledger line outlives the row"
    );
}

#[test]
fn snapshot_keeps_waiting_count_in_sync() {
    // Snapshots are full views of the registry — both agents present until
    // one is done (a real snapshot would keep showing it until
    // unregister; here "done" rows are simply dropped from the view to
    // also exercise the disappearance backstop).
    let two = |a: &str, b: &str| {
        std::sync::Arc::new(RegistrySnapshot {
            agents: vec![
                AgentSnapshot {
                    path: "root/a".into(),
                    status: a.into(),
                    running_secs: None,
                    last_activity_secs: None,
                    tool_calls: 0,
                    task: None,
                    pending_results: 0,
                    last_error: None,
                },
                AgentSnapshot {
                    path: "root/b".into(),
                    status: b.into(),
                    running_secs: None,
                    last_activity_secs: None,
                    tool_calls: 0,
                    task: None,
                    pending_results: 0,
                    last_error: None,
                },
            ],
        })
    };

    let mut app = App::new();
    app.status = AgentStatus::Waiting { running: 2, bg: 0 };
    app.handle_event(TuiEvent::Lifecycle(two("running", "running")));
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 2, bg: 0 }
    ));

    app.handle_event(TuiEvent::Lifecycle(two("done", "running")));
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 1, bg: 0 }
    ));

    app.handle_event(TuiEvent::Lifecycle(two("done", "done")));
    assert!(matches!(app.status, AgentStatus::Idle));
}
