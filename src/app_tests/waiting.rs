//! Fan-in and background-task waiting states.

use super::super::*;
use super::common::*;

// ── Fan-in waiting state (turn ends while sub-agents run) ──────────────────

#[test]
fn turn_done_with_running_children_enters_waiting_state() {
    let mut app = App::new();
    app.running = true;
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "working")));
    app.handle_event(TuiEvent::Runtime(child_text("root/b", "working")));
    // Root finishes its turn but children are still out — root RunFinished
    // must keep the panel alive and flip to Waiting.
    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    assert_eq!(app.sub_agents.len(), 2);
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 2, bg: 0 }
    ));

    app.handle_event(TuiEvent::TurnDone);
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("... waiting for sub-agents (2 running)"))
    );
    assert!(
        !texts.iter().any(|t| t.contains("✅ done")),
        "waiting is not done"
    );
    assert!(!app.running);
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 2, bg: 0 }
    ));
    // Panel + per-agent transcripts stay alive for inspection while waiting.
    assert_eq!(app.sub_agents.len(), 2);
    assert!(app.sub_agent_transcripts.contains_key("root/a"));
}

#[test]
fn turn_done_without_running_children_shows_done_and_clears_panel() {
    let mut app = App::new();
    app.running = true;
    app.begin_activity();
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "hi")));
    app.handle_event(TuiEvent::Runtime(run_finished(Some("root/a")))); // child done first
    // The child's terminal fact comes from the wire, not its RunFinished.
    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);
    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    app.handle_event(TuiEvent::TurnDone);
    assert!(app.last_turn_summary.is_some(), "done summary recorded");
    assert_eq!(app.status, AgentStatus::Idle);
    assert!(app.sub_agents.is_empty());
}

#[test]
fn finishing_children_updates_waiting_count_to_idle_at_zero() {
    let mut app = App::new();
    app.running = true;
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "x")));
    app.handle_event(TuiEvent::Runtime(child_text("root/b", "y")));
    app.handle_event(TuiEvent::TurnDone);
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 2, bg: 0 }
    ));

    // Watcher Progress events flip panel entries and refresh the count.
    app.mark_sub_agent_finished("root/a", SubAgentStatus::Done);
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 1, bg: 0 }
    ));
    assert_eq!(
        app.sub_agents.get("root/a").map(|s| &s.status),
        Some(&SubAgentStatus::Done)
    );

    app.mark_sub_agent_finished("root/b", SubAgentStatus::Done);
    assert_eq!(app.status, AgentStatus::Idle);
}

#[test]
fn batch_inject_marks_all_children_finished() {
    let mut app = App::new();
    app.running = true;
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "x")));
    app.handle_event(TuiEvent::Runtime(child_text("root/b", "y")));
    app.handle_event(TuiEvent::TurnDone);
    app.mark_all_sub_agents_finished();
    assert!(
        app.sub_agents
            .values()
            .all(|s| s.status == SubAgentStatus::Done)
    );
    assert_eq!(app.status, AgentStatus::Idle);
}

#[test]
fn waiting_status_line_mentions_running_count() {
    let mut app = App::new();
    app.status = AgentStatus::Waiting { running: 3, bg: 0 };
    let line = app.status_line();
    assert!(line.contains('3'), "count missing: {line}");
    assert!(line.contains("waiting"), "waiting wording missing: {line}");
}

// ── Background-task waiting state (turn ends while bg tasks run) ────────────

fn running_bg_task(app: &mut App, id: &str, command: &str) {
    running_bg_task_with_timeout(app, id, command, 120_000);
}

/// Daemon-style entry (`timeout_ms == 0`): a server left running on purpose.
fn running_daemon_task(app: &mut App, id: &str, command: &str) {
    running_bg_task_with_timeout(app, id, command, 0);
}

fn running_bg_task_with_timeout(app: &mut App, id: &str, command: &str, timeout_ms: u64) {
    app.background_tasks.insert(
        id.to_string(),
        BackgroundTaskEntry {
            id: id.to_string(),
            command: command.to_string(),
            timeout_ms,
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            finished_at: None,
            reported: false,
            output_tail: String::new(),
            consumed: false,
        },
    );
}

#[test]
fn turn_done_with_running_bg_tasks_enters_waiting_not_idle() {
    let mut app = App::new();
    app.running = true;
    running_bg_task(&mut app, "bg_aaaa1111", "cargo test");
    running_bg_task(&mut app, "bg_bbbb2222", "npm run build");

    // Root finishes its turn while the two background tasks are still out —
    // the status must NOT drop to Idle ("done"), it must wait on the bg
    // tasks the same way it waits on sub-agents.
    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 0, bg: 2 }
    ));

    app.handle_event(TuiEvent::TurnDone);
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("waiting for background tasks") && t.contains('2'))
    );
    assert!(
        !texts.iter().any(|t| t.contains("✅ done")),
        "waiting is not done"
    );
    assert!(!app.running);
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 0, bg: 2 }
    ));
    // Status bar: "waiting for background tasks", not "done".
    let line = app.status_line();
    assert!(
        line.contains("background tasks"),
        "bg wording missing: {line}"
    );
    assert!(line.contains('2'), "bg count missing: {line}");
}

#[test]
fn turn_done_with_children_and_bg_tasks_waits_for_both() {
    let mut app = App::new();
    app.running = true;
    app.handle_event(TuiEvent::Runtime(child_text("root/a", "working")));
    running_bg_task(&mut app, "bg_aaaa1111", "sleep 5");

    app.handle_event(TuiEvent::TurnDone);
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 1, bg: 1 }
    ));
    let line = app.status_line();
    assert!(
        line.contains("sub-agents") && line.contains("background tasks"),
        "both: {line}"
    );
}

#[test]
fn bg_task_finishing_while_waiting_updates_count() {
    let mut app = App::new();
    app.status = AgentStatus::Waiting { running: 0, bg: 1 };

    // Registry flip → reconcile → refresh (the production trigger path).
    let registry = BackgroundTaskRegistry::new(4);
    app.set_background_registry(registry.clone());
    let token = tokio_util::sync::CancellationToken::new();
    let id = registry
        .register("cargo test", None, token, None, 120_000)
        .unwrap();
    assert!(app.reconcile_background_tasks(), "new task is a change");
    assert!(matches!(
        app.status,
        AgentStatus::Waiting { running: 0, bg: 1 }
    ));

    registry.finish(&id, Some(0));
    assert!(app.reconcile_background_tasks(), "status flip is a change");
    assert_eq!(app.status, AgentStatus::Idle, "last bg task done → idle");
}

#[test]
fn daemon_only_settle_is_done_plus_service_note_not_waiting() {
    // Session 20260922_6d262d0f: the turn ended with a `mvn spring-boot:run`
    // daemon (`timeout_ms: 0`) up, and the TUI promised an automatic report
    // for a process that never completes. A daemon is steady state: settle
    // as done and note the service, never "waiting".
    let mut app = App::new();
    app.running = true;
    app.begin_activity();
    running_daemon_task(&mut app, "bg_daemon01", "mvn spring-boot:run");

    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    app.handle_event(TuiEvent::TurnDone);

    assert_eq!(
        app.status,
        AgentStatus::Idle,
        "daemon alone = the agent can rest"
    );
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert!(
        app.last_turn_summary.is_some(),
        "work is done: summary recorded"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("daemon") && t.contains('1')),
        "service noted: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("waiting")),
        "nothing to wait for: {texts:?}"
    );
    let line = app.status_line();
    assert!(
        line.contains("daemon"),
        "status bar keeps the service visible: {line}"
    );
    assert!(
        !line.contains("waiting"),
        "status bar must not wait: {line}"
    );
}

#[test]
fn mixed_bounded_and_daemon_settle_waits_only_for_the_bounded() {
    let mut app = App::new();
    app.running = true;
    running_bg_task(&mut app, "bg_aaaa1111", "cargo test");
    running_daemon_task(&mut app, "bg_daemon01", "mvn spring-boot:run");

    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    app.handle_event(TuiEvent::TurnDone);

    assert!(
        matches!(app.status, AgentStatus::Waiting { running: 0, bg: 2 }),
        "counter still covers everything running"
    );
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    let wait = texts
        .iter()
        .find(|t| t.contains("waiting for background tasks"))
        .expect("waiting line");
    assert!(wait.contains("1 running"), "count = bounded only: {wait}");
    assert!(wait.contains("daemon"), "daemon still listed: {wait}");
    let line = app.status_line();
    assert!(
        line.contains("waiting for background tasks") && line.contains("daemon"),
        "{line}"
    );
}

#[test]
fn idle_status_line_lists_daemons_without_waiting() {
    let mut app = App::new();
    running_daemon_task(&mut app, "bg_daemon01", "mvn spring-boot:run");
    app.status = AgentStatus::Idle;
    let line = app.status_line();
    assert!(line.starts_with("Idle"), "{line}");
    assert!(line.contains("1 daemon(s)"), "{line}");
}
