//! Steer queue submission, mirror sync, and the queued strip.

use super::super::*;

// ── Queue steering: Enter while running ──────────────────────────────────

/// Collect the echoed user-message texts in the transcript.
fn user_echoes(app: &App) -> Vec<String> {
    app.transcript
        .output
        .iter()
        .filter(|l| l.kind == LineKind::User)
        .map(|l| l.text.clone())
        .collect()
}

#[test]
fn enter_while_running_queues_instead_of_dropping() {
    let mut app = App::new();
    app.running = true;
    app.composer.insert_str("hold that thought");
    let action = app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(action, Some(Action::Steer("hold that thought".to_string())));
    assert!(
        app.composer.is_empty(),
        "the draft leaves the composer when queued"
    );
}

#[test]
fn enter_while_running_with_empty_composer_queues_nothing() {
    let mut app = App::new();
    app.running = true;
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
}

#[test]
fn enter_while_idle_still_submits() {
    let mut app = App::new();
    app.composer.insert_str("hello");
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Some(Action::Submit("hello".to_string()))
    );
}

#[test]
fn ctrl_enter_force_sends_the_draft() {
    let mut app = App::new();
    app.running = true;
    app.composer.insert_str("stop and answer");
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::CONTROL),
        Some(Action::ForceSend("stop and answer".to_string()))
    );
}

#[test]
fn ctrl_enter_with_empty_composer_flushes_the_queue() {
    let mut app = App::new();
    app.running = true;
    app.queued_steer.push("one".to_string());
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::CONTROL),
        Some(Action::ForceSend(String::new()))
    );
    // Nothing queued and nothing drafted: a no-op, never an accidental submit.
    app.queued_steer.clear();
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::CONTROL), None);
}

#[test]
fn up_takes_back_queued_messages_from_the_first_line() {
    let mut app = App::new();
    app.queued_steer = vec!["one".to_string(), "two".to_string()];
    assert_eq!(
        app.handle_key(KeyCode::Up, KeyModifiers::NONE),
        Some(Action::TakeBackQueued)
    );
}

#[test]
fn up_moves_the_cursor_first_when_not_on_the_first_line() {
    let mut app = App::new();
    app.queued_steer = vec!["one".to_string()];
    app.composer.insert_str("a\nb"); // cursor rests on row 1
    assert_eq!(app.composer.cursor().0, 1);
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.cursor().0, 0, "first Up moves the cursor");
    // Now on the first line: the next Up takes the queue back.
    assert_eq!(
        app.handle_key(KeyCode::Up, KeyModifiers::NONE),
        Some(Action::TakeBackQueued)
    );
}

// ── Queue steering: mirror sync + take-back + strip ──────────────────────

#[test]
fn sync_steer_queue_echoes_delivered_messages_exactly_once() {
    let mut app = App::new();
    app.queued_steer = vec!["one".to_string(), "two".to_string()];
    // The engine drained "one" at a tool boundary: it is now delivered.
    assert!(app.sync_steer_queue(vec!["two".to_string()]));
    assert_eq!(app.queued_steer, vec!["two".to_string()]);
    let echoes = user_echoes(&app);
    assert!(
        echoes.iter().any(|e| e.contains("one")),
        "delivered message echoed: {echoes:?}"
    );
    assert!(
        !echoes.iter().any(|e| e.contains("two")),
        "pending message not echoed yet: {echoes:?}"
    );
    // A second sync with the same snapshot is a no-op — never a double echo.
    assert!(!app.sync_steer_queue(vec!["two".to_string()]));
    assert_eq!(user_echoes(&app).len(), echoes.len());
}

#[test]
fn sync_steer_queue_adopts_engine_order() {
    let mut app = App::new();
    app.queued_steer = vec!["one".to_string()];
    // Nothing delivered; the engine queue is the source of truth.
    assert!(app.sync_steer_queue(vec!["one".to_string(), "two".to_string()]));
    assert_eq!(app.queued_steer, vec!["one".to_string(), "two".to_string()]);
    assert!(
        user_echoes(&app).is_empty(),
        "nothing delivered, nothing echoed"
    );
}

#[test]
fn take_back_into_composer_restores_above_the_draft() {
    let mut app = App::new();
    app.composer.insert_str("draft");
    app.take_back_into_composer(vec!["one".to_string(), "two".to_string()]);
    assert_eq!(app.composer.text(), "one\ntwo\ndraft");
    assert!(
        app.queued_steer.is_empty(),
        "the mirror empties with the queue"
    );
    assert!(user_echoes(&app).is_empty(), "take-back never echoes");
}

#[test]
fn queued_strip_lines_bound_rows_and_mark_the_rest() {
    let mut app = App::new();
    app.queued_steer = (0..3).map(|i| format!("m{i}")).collect();
    app.queued_cmds.push("/review".to_string());
    let rows = app.queued_strip_lines();
    assert_eq!(rows.len(), 3, "capped at three rows");
    assert!(rows[0].contains("queued -> m0"), "row: {}", rows[0]);
    assert!(rows[1].contains("queued -> m1"), "row: {}", rows[1]);
    assert!(rows[2].contains("+2 more"), "tail row: {}", rows[2]);
}

#[test]
fn queued_strip_lines_mark_held_commands() {
    let mut app = App::new();
    app.queued_cmds.push("/review".to_string());
    let rows = app.queued_strip_lines();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].contains("queued after turn -> /review"),
        "row: {}",
        rows[0]
    );
}

#[test]
fn queued_strip_lines_elide_long_first_lines() {
    let mut app = App::new();
    let long = "x".repeat(200);
    app.queued_steer.push(long);
    let rows = app.queued_strip_lines();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].ends_with("..."), "row: {}", rows[0]);
    assert!(rows[0].chars().count() <= 60 + " queued -> ".chars().count() + 3);
}

#[test]
fn queued_strip_is_empty_without_a_queue() {
    let app = App::new();
    assert!(app.queued_strip_lines().is_empty());
}

// ── Held commands: dedupe and one-run-per-flush ──────────────────────────

/// protects: a hammered `/` command is held once. Each held command replays
/// as its own turn at the end of the current one — an interactive one like
/// `/model` replays its whole pick + persisted switch — so a duplicate hold
/// would fire a second pick right after the first and flip-flop the switch
/// back. fails_when: the same command queued twice runs twice, or a
/// distinct command is refused. no_claim: what any command does when run.
#[test]
fn holding_the_same_command_twice_queues_it_once() {
    let mut app = App::new();
    assert!(
        app.hold_command("/model".to_string()),
        "the first hold lands"
    );
    assert!(
        !app.hold_command("/model".to_string()),
        "the impatient repeat is refused"
    );
    assert!(
        app.hold_command("/help".to_string()),
        "a different command still queues"
    );
    assert_eq!(
        app.queued_cmds,
        vec!["/model".to_string(), "/help".to_string()]
    );
}

/// protects: the idle-frame flush fires exactly one run per pass —
/// undelivered steering first (merged as one run), then the oldest held
/// command — and the rest stay queued for later idle frames. Firing the
/// whole held queue in one burst is what stacked two pick modals
/// back-to-back and double-wrote the model switch. fails_when: one call
/// drains the queue, order is wrong, or an empty queue reports a run.
/// no_claim: when the pump chooses to call again.
#[test]
fn take_next_queued_run_fires_one_ride_per_call_in_order() {
    let mut app = App::new();
    assert_eq!(
        app.take_next_queued_run(vec![]),
        None,
        "empty queue, no run"
    );
    app.hold_command("/model".to_string());
    app.hold_command("/review".to_string());
    assert_eq!(
        app.take_next_queued_run(vec!["hint one".to_string(), "hint two".to_string()]),
        Some("hint one\nhint two".to_string()),
        "undelivered steering rides first, merged as one run"
    );
    assert_eq!(app.take_next_queued_run(vec![]), Some("/model".to_string()));
    assert_eq!(
        app.take_next_queued_run(vec![]),
        Some("/review".to_string()),
        "the second command waits its own pass"
    );
    assert_eq!(
        app.take_next_queued_run(vec![]),
        None,
        "the queue is drained"
    );
}

/// protects: the mid-turn lane claims one copy of a command input — a
/// hammered Enter must not spawn a second pick. fails_when: the repeat
/// claims a second slot. no_claim: what the lane does with the claim.
#[test]
fn the_mid_turn_lane_claims_one_copy_of_a_command() {
    let app = App::new();
    assert!(app.claim_mid_command("/model"), "first press claims");
    assert!(!app.claim_mid_command("/model"), "the repeat is refused");
    assert!(
        app.claim_mid_command("/lite"),
        "a different command is free"
    );
}
