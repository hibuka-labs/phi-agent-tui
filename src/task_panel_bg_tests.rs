//! Background shell task rows: reconcile upserts, the TERMINAL_INSERT_WINDOW
//! guard against resurrecting swept rows, wake debt (`may_reap`, issue #29)
//! and sweep-owned reaping (Rev 2026-11-04 — the clock reaps nothing).

use std::time::{Duration, Instant};

use phi_kernel_tools::background_shell::{BackgroundTaskRegistry, BackgroundTaskStatus};

use crate::app::{AgentStatus, App, BackgroundTaskEntry};

// ── Background Task Reap Tests ─────────────────────────────────────────────

#[test]
fn background_tasks_are_reaped_by_the_sweep_not_the_clock() {
    // The clock no longer reaps (the old 3s auto-reap is retired): the
    // registry keeps reporting the task and the map follows the death
    // sweeps — a terminal entry leaves when a sweep runs and the wake has
    // taken its outcome (`may_reap`, issue #29).

    let registry = BackgroundTaskRegistry::new(4);
    let mut app = App::new();
    app.set_background_registry(registry.clone());

    // Register a background task
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let task_id = registry
        .register("echo test", None, cancel_token, None, 120_000)
        .unwrap();

    // First reconcile: task should appear in app.background_tasks
    let changed = app.reconcile_background_tasks();
    assert!(changed, "first reconcile should detect new task");
    assert_eq!(app.background_tasks.len(), 1);
    assert_eq!(
        app.background_tasks[&task_id].status,
        BackgroundTaskStatus::Running
    );

    // Finish the task in the registry
    registry.update_status(&task_id, BackgroundTaskStatus::Done);

    // Reconcile: task should be updated to Done, finished_at set to now
    let changed = app.reconcile_background_tasks();
    assert!(changed, "reconcile should detect status change");
    assert_eq!(app.background_tasks.len(), 1);
    assert_eq!(
        app.background_tasks[&task_id].status,
        BackgroundTaskStatus::Done
    );

    // Age is not a reap condition any more: backdate and tick away — the
    // entry stays as long as no sweep runs.
    app.background_tasks.get_mut(&task_id).unwrap().finished_at =
        Some(Instant::now() - Duration::from_secs(4));
    let changed = app.reconcile_background_tasks();
    assert!(!changed, "unchanged reconcile stays quiescent");
    assert_eq!(app.background_tasks.len(), 1, "no clock reap");

    // An unreported outcome still owes the agent a wake (issue #29) — the
    // sweep must not eat it either.
    assert!(!app.turn_end_sweep(), "unreported outcome is spared");
    assert_eq!(
        app.background_tasks.len(),
        1,
        "unreported task must not be reaped"
    );

    // Once the wake has taken it, the sweep reaps it.
    app.background_tasks.get_mut(&task_id).unwrap().reported = true;
    assert!(app.turn_end_sweep(), "the sweep reaps the reported task");
    assert_eq!(app.background_tasks.len(), 0, "task should be reaped");
}

#[test]
fn background_task_opens_the_task_panel_as_a_row() {
    // Panel policy (2026-10-29): background shell tasks are switchable rows —
    // selecting one shows its live output tail (the "can't see what the
    // background work is doing" gap). Rev 2026-11-04: the row hides the
    // moment it lands terminal (hide-on-complete 完成即隐) and a death sweep takes the entry.

    let registry = BackgroundTaskRegistry::new(4);
    let mut app = App::new();
    app.set_background_registry(registry.clone());

    // Initially: no task panel
    assert!(!app.should_show_task_panel(), "no panel when no tasks");

    // Register a background task
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let task_id = registry
        .register("sleep 5", None, cancel_token, None, 120_000)
        .unwrap();

    // Reconcile: the task opens the panel as a background row.
    app.reconcile_background_tasks();
    assert_eq!(app.background_tasks.len(), 1);
    assert!(
        app.should_show_task_panel(),
        "bg task opens the panel (it is a row now)"
    );
    assert_eq!(
        app.task_rows(),
        vec![crate::task_panel::TaskRow::Background(task_id.clone())],
        "the row is the background task"
    );

    // Finish the task: an untouched terminal row hides the moment it lands
    // (hide-on-complete 完成即隐) — the map keeps the outcome for the wake.
    registry.update_status(&task_id, BackgroundTaskStatus::Done);
    app.reconcile_background_tasks();
    assert!(
        !app.should_show_task_panel(),
        "the face has nothing left to show"
    );
    assert!(
        app.background_tasks.contains_key(&task_id),
        "the outcome is kept until the wake reports it"
    );

    // The sweep takes the entry once the wake has taken the outcome
    // (`may_reap`, issue #29).
    app.background_tasks.get_mut(&task_id).unwrap().reported = true;
    assert!(app.turn_end_sweep(), "the sweep still cleans up");
    assert_eq!(app.background_tasks.len(), 0, "reap still works");
}

#[test]
fn task_rows_put_sub_agents_before_background_tasks() {
    let mut app = App::new();
    app.track_agent(Some("root/alpha"));
    app.track_agent(Some("root/beta"));
    // Background rows come from the map directly (registry plumbing is
    // covered by the reconcile tests).
    app.background_tasks.insert(
        "bg1".to_string(),
        BackgroundTaskEntry {
            id: "bg1".to_string(),
            command: "cargo test".to_string(),
            timeout_ms: 0,
            status: BackgroundTaskStatus::Running,
            started_at: Instant::now(),
            finished_at: None,
            reported: false,
            output_tail: String::new(),
            consumed: false,
        },
    );

    assert_eq!(
        app.task_rows(),
        vec![
            crate::task_panel::TaskRow::SubAgent("root/alpha".to_string()),
            crate::task_panel::TaskRow::SubAgent("root/beta".to_string()),
            crate::task_panel::TaskRow::Background("bg1".to_string()),
        ]
    );
    assert_eq!(app.task_row_count(), 3);
}

#[test]
fn background_tail_refreshes_while_running() {
    let registry = BackgroundTaskRegistry::new(4);
    let mut app = App::new();
    app.set_background_registry(registry.clone());

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let task_id = registry
        .register("sleep 5", None, cancel_token, None, 120_000)
        .unwrap();
    app.reconcile_background_tasks();
    assert_eq!(app.background_tasks[&task_id].output_tail, "");

    // Output arriving while the task runs lands in the tail per tick — the
    // row's inspection view is live, not terminal-only.
    registry.append_output(&task_id, Some("line one"), None);
    assert!(app.reconcile_background_tasks(), "tail moved the frame");
    assert!(
        app.background_tasks[&task_id]
            .output_tail
            .contains("line one"),
        "tail: {:?}",
        app.background_tasks[&task_id].output_tail
    );

    // Quiet tick: nothing changed, nothing dirties the frame.
    assert!(
        !app.reconcile_background_tasks(),
        "unchanged tail must not dirty every tick"
    );

    registry.append_output(&task_id, Some("line two"), None);
    assert!(app.reconcile_background_tasks());
    let tail = &app.background_tasks[&task_id].output_tail;
    assert!(
        tail.contains("line one") && tail.contains("line two"),
        "{tail:?}"
    );
}

#[test]
fn reaped_task_is_not_readded_by_later_reconciles() {
    // Regression: reap → re-add → reap loop. `snapshot_all` keeps finished
    // tasks until the registry's own GC TTL expires, so after a sweep drops
    // a task the next tick's snapshot still reports it. Reconcile must not
    // resurrect it: the upsert skips terminal snapshots whose finished_at
    // is already past the 3s insert window (TERMINAL_INSERT_WINDOW).
    //
    // Uses a real sleep (not app-side backdating): the guard keys off the
    // registry's own finished_at, which in production equals the panel's —
    // both are taken at the same finish instant.
    let registry = BackgroundTaskRegistry::new(4);
    let mut app = App::new();
    app.set_background_registry(registry.clone());

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let task_id = registry
        .register("sleep 30", None, cancel_token, None, 120_000)
        .unwrap();

    // Task appears, then finishes; panel reflects Done with the registry's
    // finished_at.
    assert!(
        app.reconcile_background_tasks(),
        "first reconcile detects the new task"
    );
    registry.update_status(&task_id, BackgroundTaskStatus::Done);
    assert!(
        app.reconcile_background_tasks(),
        "reconcile detects the status change"
    );
    assert_eq!(
        app.background_tasks[&task_id].status,
        BackgroundTaskStatus::Done
    );

    // The wake takes the outcome before the sweep — only reported entries
    // are reapable (`may_reap`, issue #29).
    app.background_tasks.get_mut(&task_id).unwrap().reported = true;

    // Past the 3s insert window: the sweep drops the task …
    std::thread::sleep(Duration::from_millis(3100));
    assert!(app.turn_end_sweep(), "the sweep drops the reported task");
    assert!(app.background_tasks.is_empty(), "task should be reaped");

    // … and every subsequent tick stays quiescent. The registry still holds
    // the task (its GC TTL is 5 min), but the panel must not re-add it.
    for tick in 0..3 {
        assert!(
            !app.reconcile_background_tasks(),
            "reconcile {tick} must stay quiescent after reap"
        );
        assert!(
            app.background_tasks.is_empty(),
            "reaped task must not be re-added"
        );
    }
    assert!(
        !app.should_show_task_panel(),
        "panel stays hidden after reap"
    );
}

#[test]
fn stale_finished_snapshot_never_seen_is_not_inserted() {
    // A terminal task that aged past the 3s display window before any
    // reconcile ran (e.g. the UI thread was blocked) is past its window:
    // inserting it would only get it re-reaped on the same pass.
    let registry = BackgroundTaskRegistry::new(4);
    let mut app = App::new();
    app.set_background_registry(registry.clone());

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let task_id = registry
        .register("echo late", None, cancel_token, None, 120_000)
        .unwrap();
    registry.update_status(&task_id, BackgroundTaskStatus::Done);
    std::thread::sleep(Duration::from_millis(3100));

    assert!(
        !app.reconcile_background_tasks(),
        "stale finished task must not be inserted"
    );
    assert!(app.background_tasks.is_empty());
    assert!(!app.should_show_task_panel());
}

#[test]
fn unreported_outcome_survives_the_sweep_until_the_wake_reports_it() {
    // Issue #29, the "reap races ahead" half: the display reap used to eat
    // an unreported outcome — in a pass that also flipped the batch to Idle,
    // a sibling finished earlier was deleted before the wake listed it. The
    // sweeps now only drop entries that no longer owe a report (`may_reap`).
    let registry = BackgroundTaskRegistry::new(4);
    let mut app = App::new();
    app.set_background_registry(registry.clone());

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let task_id = registry
        .register("echo quiet", None, cancel_token, None, 120_000)
        .unwrap();
    registry.update_status(&task_id, BackgroundTaskStatus::Done);

    // Observed once (finished_at taken from the snapshot) — root idle, so
    // the sweep is armed.
    assert!(
        app.reconcile_background_tasks(),
        "first reconcile observes the task"
    );
    app.status = AgentStatus::Idle;

    // The sweep runs … and must not eat an outcome the wake still owes.
    assert!(!app.turn_end_sweep(), "the sweep is blocked by the debt");
    assert!(
        app.background_tasks.contains_key(&task_id),
        "unreported outcome must survive the sweep"
    );

    // The debt is paid by the report, not by the clock: the wake still lists
    // it (the decision clock moved past the aggregation quiet window).
    let (_, input) = app
        .take_bg_wake(Instant::now() + crate::bg_wake::BG_WAKE_QUIET)
        .expect("wake must still report it");
    assert!(
        input.contains(&task_id),
        "wake lists the surviving task: {input}"
    );

    // Reported → the next sweep reaps it as usual.
    assert!(app.turn_end_sweep(), "reported task is reapable");
    assert!(app.background_tasks.is_empty(), "reported task is reaped");
}
