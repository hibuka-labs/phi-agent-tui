//! Turn lifecycle, streaming basics, scrolling, and the activity clock.

use super::super::*;
use super::common::*;
use phi_agent::SessionId;
use phi_agent::UserEvent;
use phi_tui::lines::ToolState;

#[test]
fn status_tracks_state_machine() {
    let mut app = App::new();
    assert_eq!(app.status, AgentStatus::Idle);

    app.handle_event(TuiEvent::Runtime(thought("hmm")));
    assert_eq!(
        app.status,
        AgentStatus::Running {
            phase: Phase::Thinking
        }
    );

    app.handle_event(TuiEvent::Runtime(text("answer")));
    assert_eq!(
        app.status,
        AgentStatus::Running {
            phase: Phase::Streaming
        }
    );

    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    assert_eq!(
        app.status,
        AgentStatus::Running {
            phase: Phase::ToolCall {
                tool: "read_file".into()
            }
        }
    );

    app.handle_event(TuiEvent::Runtime(tool_finished("read_file", false)));
    assert_eq!(
        app.status,
        AgentStatus::Running {
            phase: Phase::Thinking
        }
    );

    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    assert_eq!(app.status, AgentStatus::Idle);
    assert!(!app.running);
}

#[test]
fn tool_calls_render_inline_in_output() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    app.handle_event(TuiEvent::Runtime(tool_finished("read_file", false)));
    app.handle_event(TuiEvent::Runtime(tool_finished("execute_command", true)));

    // Invocation + result lines land inline in `output`, in event order.
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        texts,
        vec![
            // No args to name → bare tool name; the `*` is the marker slot the
            // renderer fills from `tool_state`.
            "* read_file",
            // Adjacent to its own call → the tool name is already one line up.
            "  < done",
            "  ⛔ execute_command denied",
        ]
    );
    assert_eq!(app.transcript.output[0].kind, LineKind::Tool);
    assert_eq!(app.transcript.output[1].kind, LineKind::ToolResult);
    assert_eq!(app.transcript.output[2].kind, LineKind::Error);
}

#[test]
fn run_finished_from_sub_agent_does_not_end_turn() {
    let mut app = App::new();
    app.running = true;
    app.handle_event(TuiEvent::Runtime(run_finished(Some("sub/1"))));
    // Sub-agent finish must not flip the top-level status to Idle.
    assert!(app.running);
    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    // The root run is over, but `running` is cleared only by the turn
    // settling (TurnDone/TurnError) — never by a late RunFinished, which
    // would unlock the composer while the next turn is already queued.
    assert!(app.running, "only settle_after_turn clears the flag");
    assert_eq!(app.status, AgentStatus::Idle);
    app.handle_event(TuiEvent::TurnDone);
    assert!(!app.running, "settling the turn clears the flag");
}

#[test]
fn text_deltas_coalesce_into_fewer_lines() {
    let mut app = App::new();
    // Three fragments with no newline → one output line, not three.
    app.handle_event(TuiEvent::Runtime(text("hel")));
    app.handle_event(TuiEvent::Runtime(text("lo ")));
    app.handle_event(TuiEvent::Runtime(text("world")));
    app.handle_event(TuiEvent::Runtime(tool_started("build"))); // flush
    let normals: Vec<_> = app
        .transcript
        .output
        .iter()
        .filter(|l| l.kind == LineKind::Normal)
        .collect();
    assert_eq!(normals.len(), 1);
    assert_eq!(normals[0].text, "hello world");
}

#[test]
fn text_with_newline_splits_lines() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(text("a\nb")));
    app.handle_event(TuiEvent::Runtime(tool_started("build")));
    let normals: Vec<_> = app
        .transcript
        .output
        .iter()
        .filter(|l| l.kind == LineKind::Normal)
        .collect();
    // The full raw text is stored as ONE OutputLine; the renderer handles
    // markdown parsing and wrapping at display time.
    assert_eq!(normals.len(), 1);
    assert_eq!(normals[0].text, "a\nb");
}

#[test]
fn thought_renders_before_text() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(thought("thinking")));
    app.handle_event(TuiEvent::Runtime(text("answer")));
    app.handle_event(TuiEvent::Runtime(run_finished(None)));
    assert_eq!(app.transcript.output[0].kind, LineKind::Thought);
    assert_eq!(app.transcript.output[0].text, "thinking");
    assert_eq!(app.transcript.output[1].kind, LineKind::Normal);
    assert_eq!(app.transcript.output[1].text, "answer");
}

#[test]
fn submit_requires_nonempty_and_not_running() {
    let mut app = App::new();
    // Empty composer → no submit.
    app.composer.clear();
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);

    // Non-empty → Submit. The `running` flag is raised by the *send* side
    // (run.rs, right after `Cmd::Run` is queued), not by key handling — so
    // intercepted submits (/resume, /upgrade) can never leave the composer
    // locked with the flag up and no run in flight.
    let action = submit(&mut app, "do a thing");
    assert_eq!(action, Action::Submit("do a thing".to_string()));

    // Already running → Enter queues a steer (delivered at the next tool
    // boundary), never a silent drop and never a Submit — the send side
    // raised the flag, so only `Steer`/`ForceSend` may reach the engine.
    app.running = true;
    app.composer.insert_str("second");
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Some(Action::Steer("second".to_string()))
    );
}

/// Regression (session 20261013_075bb995): `/resume` is intercepted before
/// the `Cmd::Run` send. Enter used to commit the turn state in key handling,
/// so cancelling the picker left `status = Running { Thinking }` and a running
/// activity clock with `total_turns == 0` — the status bar span`thinking... - 3m10s` for minutes with no run in flight, and the transcript
/// echoed `> /resume` as if the message had been sent.
#[test]
fn intercepted_submit_commits_no_turn_state() {
    let mut app = App::new();
    app.composer.insert_str("/resume");

    // Exactly what run.rs does for an intercepted submit: key handling yields
    // the action, then the loop branches into the interceptor and never
    // reaches the send site.
    let action = app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(action, Some(Action::Submit("/resume".to_string())));

    assert!(
        matches!(app.status, AgentStatus::Idle),
        "intercepted submit must not start a turn, got {:?}",
        app.status
    );
    assert!(
        app.activity_since.is_none(),
        "intercepted submit must not start the activity clock"
    );
    assert!(
        !app.is_active(),
        "no run in flight — the liveness tick must not spin a phantom spinner"
    );
    let users: Vec<&str> = app
        .transcript
        .output
        .iter()
        .filter(|l| l.kind == LineKind::User)
        .map(|l| l.text.as_str())
        .collect();
    assert!(
        users.is_empty(),
        "intercepted submit must not echo a message that was never sent, got {users:?}"
    );

    // The composer is still usable: `running` was never raised either.
    assert!(!app.running, "composer must not be locked");
}

/// Regression (user report 2026-09-16, round 3): after resuming, the user
/// reads the history scrolled up; on submit the reply must stream into
/// view, not land below the fold ("no response after typing" was the reply being
/// rendered off-screen while the viewport stayed pinned at the head).
#[test]
fn submit_scrolls_back_to_bottom_so_reply_is_visible() {
    let mut app = App::new();
    for i in 0..100 {
        app.push_system(&format!("history line {i}"));
    }
    // Scroll up into the history (as after reading a resumed conversation).
    assert!(app.scroll_up());
    assert!(!app.viewport.follow_bottom, "precondition: scrolled up");

    let action = submit(&mut app, "好的");
    assert!(matches!(action, Action::Submit(_)));

    // Submit re-enters follow-bottom: the newest lines (the user echo now,
    // the reply as it streams) are what's on screen.
    assert!(app.viewport.follow_bottom, "submit must follow the bottom");
    assert!(!app.viewport.pin_top);
    let total = app.transcript.len();
    assert_eq!(
        app.viewport.window_range(total, 40),
        total.saturating_sub(40)..total,
        "the reply lands on screen, not below the fold"
    );
}

/// One PageUp/PageDown press moves half a screen, not one line: a resumed
/// conversation runs to hundreds of lines, and 1-line paging made the
/// history effectively unnavigable ("only the first screen shows").
#[test]
fn pagedown_pages_half_a_screen() {
    let mut app = App::new();
    for i in 0..200 {
        app.push_system(&format!("history line {i}"));
    }
    let total = app.transcript.len();
    assert!(
        total > 60,
        "precondition: enough content to page (got {total})"
    );
    // Emulate a rendered frame so the viewport knows its geometry.
    app.viewport.viewport_height = 40;
    app.viewport.rendered_total = total;
    assert_eq!(app.viewport.page_step(), 20, "half of the 40-row viewport");

    // Fresh boot follows the bottom: PgDn there is a no-op.
    assert!(!app.scroll_down(), "already at the bottom: no movement");

    // One PgUp press leaves follow-bottom and jumps a full half-screen.
    assert!(app.scroll_up());
    assert!(!app.viewport.follow_bottom);
    let range = app.viewport.window_range(total, 40);
    assert_eq!(range.start, total - 40 - 20, "one PgUp = half a screen");

    // PgDn walks back down and re-enters follow at the bottom.
    assert!(app.scroll_down());
    assert!(app.viewport.follow_bottom);
    assert_eq!(
        app.viewport.window_range(total, 40),
        total.saturating_sub(40)..total,
        "back at the newest lines"
    );
}

#[test]
fn submit_echoes_user_message() {
    let mut app = App::new();
    let action = submit(&mut app, "hello world");
    assert_eq!(action, Action::Submit("hello world".to_string()));
    let users: Vec<&str> = app
        .transcript
        .output
        .iter()
        .filter(|l| l.kind == LineKind::User)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(users, vec!["> hello world"]);
}

#[test]
fn progress_updates_and_clears_live_progress() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(tool_started("execute_command")));
    // The status line now carries a spinner frame + elapsed readout (the
    // liveness tick redraws it); assert the stable parts.
    let line = app.status_line();
    assert!(line.contains("🔧 execute_command"), "got: {line}");
    assert!(line.contains("(Ctrl+C cancel)"), "got: {line}");

    let prog = RuntimeEvent::UserEvent {
        session_id: SessionId::new(1),
        event: UserEvent::Progress {
            text: "Compiling phimint v0.1.0".to_string(),
        },
        agent_id: None,
        trace_id: None,
    };
    app.handle_event(TuiEvent::Runtime(prog));
    let line = app.status_line();
    assert!(
        line.contains("🔧 execute_command: Compiling phimint v0.1.0"),
        "got: {line}"
    );

    // A new tool call drops the previous tool's live progress.
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    let line = app.status_line();
    assert!(line.contains("🔧 read_file"), "got: {line}");
    assert!(
        !line.contains("Compiling"),
        "stale progress must drop, got: {line}"
    );
}

#[test]
fn spawn_agent_invocation_line_is_compacted() {
    let mut app = App::new();
    let args = serde_json::json!({
        "task_name": "analyze-pi",
        "task": "分析 /Users/me/pi 工程的 agent 循环\n然后还要看工具系统\n报告带文件行号"
    })
    .to_string();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "spawn_agent".to_string(),
        args_json: args,
        agent_id: None,
        trace_id: None,
    }));
    let last = app.transcript.output.last().unwrap();
    assert!(
        last.text
            .starts_with("* spawn_agent analyze-pi - 分析 /Users/me/pi 工程的 agent 循环"),
        "got: {}",
        last.text
    );
    assert!(
        !last.text.contains("然后还要看"),
        "must not dump the full task"
    );
}

#[test]
fn tool_args_drop_schema_default_noise() {
    let mut app = App::new();
    app.set_workspace_root(std::path::PathBuf::from("/work"));
    // Every knob spelled out at its default is one fact wearing decorations:
    // the row keeps the fact only.
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "execute_command",
        r#"{"background":false,"command":"cargo build","timeout_ms":120000,"working_dir":"/work"}"#,
    )));
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "list_files",
        r#"{"limit":500,"path":".","recursive":false}"#,
    )));
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "read_file",
        r#"{"limit":2000,"offset":0,"path":"src/main.rs"}"#,
    )));
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        texts,
        vec![
            "* execute_command  cargo build",
            "* list_files",
            "* read_file  main.rs",
        ]
    );
}

#[test]
fn tool_args_keep_non_default_choices() {
    let mut app = App::new();
    // Choices that differ from the schema default *are* the call.
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "execute_command",
        r#"{"background":true,"command":"npm run dev","timeout_ms":300000}"#,
    )));
    // …while the background default (0 = never kill) still drops.
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "execute_command",
        r#"{"background":true,"command":"vite","timeout_ms":0}"#,
    )));
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "list_files",
        r#"{"limit":50,"path":"src","recursive":true}"#,
    )));
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "read_file",
        r#"{"limit":50,"offset":10,"path":"src/main.rs"}"#,
    )));
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        texts,
        vec![
            "* execute_command  npm run dev  background  timeout=5m",
            "* execute_command  vite  background",
            "* list_files  limit=50  src  recursive",
            "* read_file  main.rs from L11 to L60",
        ]
    );
}

#[test]
fn tool_args_humanize_durations_and_flags() {
    let mut app = App::new();
    // Durations speak the unit a human would say; set flags are the bare
    // adjective they are.
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "task_output",
        r#"{"task_id":"bg_84bb8d8c","timeout_ms":180000,"wait":true}"#,
    )));
    // Sub-minute and odd milliseconds keep the honest `ms`/`s` spelling.
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "task_output",
        r#"{"task_id":"bg_1","timeout_ms":12000}"#,
    )));
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "task_output",
        r#"{"task_id":"bg_2","timeout_ms":500}"#,
    )));
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        texts,
        vec![
            "* task_output  bg_84bb8d8c  timeout=3m  wait",
            "* task_output  bg_1  timeout=12s",
            "* task_output  bg_2  timeout=500ms",
        ]
    );
}

#[test]
fn result_head_projects_status_envelopes() {
    let mut app = App::new();
    // The envelope head is machine shorthand; the meta row spells it out.
    app.handle_event(TuiEvent::Runtime(tool_started("execute_command")));
    app.handle_event(TuiEvent::Runtime(tool_finished_with_summary(
        "execute_command",
        "[Command Failed (exit code: 1)]\ncommand: false\nstdout:\n(empty)",
    )));
    app.handle_event(TuiEvent::Runtime(tool_started("execute_command")));
    app.handle_event(TuiEvent::Runtime(tool_finished_with_summary(
        "execute_command",
        "[Command Timed Out after 12000ms]\ncommand: sleep 5",
    )));
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        texts,
        vec![
            "* execute_command",
            "  < failed (exit 1)",
            "* execute_command",
            "  < timed out (12s)",
        ]
    );
}

#[test]
fn result_drops_name_inside_the_call_block() {
    let mut app = App::new();
    // Under its own call the tool name is on the row above — chrome.
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    app.handle_event(TuiEvent::Runtime(tool_finished_with_summary(
        "read_file",
        "file body",
    )));
    // Far from its call (prose landed between) the name earns its columns.
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    app.handle_event(TuiEvent::Runtime(text("a comment")));
    app.flush_pending();
    app.handle_event(TuiEvent::Runtime(tool_finished_with_summary(
        "read_file",
        "file body",
    )));
    let texts: Vec<&str> = app
        .transcript
        .output
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        texts,
        vec![
            "* read_file",
            "  < file body",
            "* read_file",
            "a comment",
            "  < read_file  file body",
        ]
    );
}

#[test]
fn parallel_settle_pairs_by_tool_name() {
    let mut app = App::new();
    // Two calls in flight at once. The first to finish must settle its OWN
    // row: name-matched pairing, not "last running row" — that heuristic
    // crossed the state glyphs and the name decision (feed.xml/task_output
    // inversion).
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "execute_command",
        r#"{"command":"echo feed"}"#,
    )));
    app.handle_event(TuiEvent::Runtime(tool_started_with(
        "task_output",
        r#"{"task_id":"bg_1"}"#,
    )));
    app.handle_event(TuiEvent::Runtime(tool_finished_with_summary(
        "execute_command",
        "feed.xml content",
    )));
    let (exec_state, task_state) = (
        app.transcript.output[0].tool_state,
        app.transcript.output[1].tool_state,
    );
    assert_eq!(
        exec_state,
        Some(ToolState::Done),
        "first finisher settles its own row"
    );
    assert_eq!(
        task_state,
        Some(ToolState::Running),
        "sibling call is untouched"
    );
    // In the same call block the sibling result keeps its name — the rows
    // below the block are interleaved, the name is the only attribution.
    let last = app.transcript.output.last().unwrap();
    assert_eq!(last.text, "  < execute_command  feed.xml content");
}

#[test]
fn activity_clock_follows_root_status() {
    let mut app = App::new();
    assert!(!app.is_active());
    assert_eq!(app.spinner_char(), "⠋"); // idle: static first frame
    assert_eq!(app.activity_suffix(), "");

    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    assert!(app.is_active());
    assert!(app.activity_since.is_some());
    let line = app.status_line();
    assert!(
        line.contains(" - "),
        "running status carries elapsed, got: {line}"
    );

    // Turn settles with no children → Idle clears the clock.
    app.settle_after_turn(TurnEnd::Done);
    assert!(!app.is_active());
    assert!(app.activity_since.is_none());
    assert_eq!(app.spinner_char(), "⠋");
}

#[test]
fn submit_starts_activity_clock_and_keeps_origin() {
    let mut app = App::new();
    assert!(app.activity_since.is_none(), "idle: no stretch");

    let _ = submit(&mut app, "hello");
    // The stretch's clock starts at the keystroke, not at the engine's first
    // event: a model that thinks for half a minute before its first delta must
    // not render a frozen spinner with no elapsed readout (session
    // 20261006_9264ba3e).
    let origin = app.activity_since.expect("submit must start the clock");

    // A phase change inside the stretch keeps its origin — the elapsed readout
    // measures the whole stretch, not the time since the last event.
    app.handle_event(TuiEvent::Runtime(tool_started("read_file")));
    assert_eq!(
        app.activity_since,
        Some(origin),
        "tool events must not restart the stretch clock"
    );
}

#[test]
fn cancel_rescue_un_echoes_and_returns_the_draft() {
    let mut app = App::new();
    let before = app.transcript.output.len();
    submit_rescuable(&mut app, "hello world");
    assert!(
        app.transcript.output.len() > before,
        "the echo is on screen"
    );
    // The run is now in flight; nothing was produced → Ctrl+C takes the
    // message back as if never sent.
    app.running = true;
    assert_eq!(app.take_cancel_rescue().as_deref(), Some("hello world"));
    assert_eq!(
        app.transcript.output.len(),
        before,
        "the echo is rolled back with the rescue"
    );
    assert!(
        app.activity_since.is_none(),
        "the phantom turn leaves no clock"
    );
}

#[test]
fn cancel_rescue_refuses_once_output_landed() {
    let mut app = App::new();
    let before = app.transcript.output.len();
    submit_rescuable(&mut app, "hello world");
    app.running = true;
    // The model answered: the turn is part of the conversation now.
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        text: "answer".to_string(),
        kind: LineKind::Normal,
        detail: None,
        tool_state: None,
    });
    assert_eq!(app.take_cancel_rescue(), None);
    assert!(
        app.transcript.output.len() > before,
        "nothing is rolled back once the model produced output"
    );
}

#[test]
fn cancel_rescue_refuses_with_a_live_stream_tail() {
    let mut app = App::new();
    submit_rescuable(&mut app, "hello world");
    app.running = true;
    // Uncommitted streaming text is still produced text (CC: no text →
    // restore; streamed-but-unflushed counts).
    app.handle_event(TuiEvent::Runtime(text("par")));
    assert_eq!(app.take_cancel_rescue(), None);
}

#[test]
fn cancel_rescue_requires_the_turn_in_flight() {
    let mut app = App::new();
    let before = app.transcript.output.len();
    submit_rescuable(&mut app, "hello world");
    // No run is in flight — there is nothing to cancel and the echo must
    // survive.
    assert_eq!(app.take_cancel_rescue(), None);
    assert!(app.transcript.output.len() > before);
}

#[test]
fn synthetic_turns_arm_no_rescue() {
    let mut app = App::new();
    // A root submit arms the rescue...
    submit_rescuable(&mut app, "hello world");
    // ...but a synthetic turn (background wake / child fan-in) supersedes it:
    // its echo is engine talk, never a typo.
    app.begin_user_turn("background wake");
    app.running = true;
    assert_eq!(app.take_cancel_rescue(), None);
    // And a synthetic turn arms nothing on its own.
    let mut app = App::new();
    app.begin_user_turn("background wake");
    app.running = true;
    assert_eq!(app.take_cancel_rescue(), None);
}

#[test]
fn a_rescued_turn_settles_quiet_and_a_real_cancel_says_so() {
    let mut app = App::new();
    let before = app.transcript.output.len();
    submit_rescuable(&mut app, "typo message");
    app.running = true;
    assert!(app.take_cancel_rescue().is_some());
    // The late RunCancelled settles the turn without a "⏹ cancelled" row...
    app.handle_event(TuiEvent::Runtime(run_cancelled(None)));
    assert_eq!(
        app.transcript.output.len(),
        before,
        "a rescued cancel adds no rows"
    );
    assert!(!app.pending_cancel);
    // ...and the turn end appends no "crunched …" summary either.
    app.handle_event(TuiEvent::TurnDone);
    assert_eq!(app.transcript.output.len(), before);
    assert!(!app.running);

    // A real cancel is part of the conversation and says so.
    submit_rescuable(&mut app, "keep me");
    app.running = true;
    app.handle_event(TuiEvent::Runtime(run_cancelled(None)));
    assert_eq!(
        app.transcript.output.last().unwrap().kind,
        LineKind::Cancelled
    );
    app.handle_event(TuiEvent::TurnDone);
    assert!(
        app.transcript
            .output
            .last()
            .unwrap()
            .text
            .contains("cancelled"),
        "the turn-final verdict reads cancelled"
    );
}

/// The regression behind "a loop that never finished settled as done": a
/// mid-run cancel whose engine `RunCancelled` bus hop never landed must still
/// settle the turn as cancelled — `TurnCancelled` alone stamps the verdict.
#[test]
fn turn_cancelled_stamps_the_verdict_without_the_engine_event() {
    let mut app = App::new();
    submit_rescuable(&mut app, "run the loop");
    app.running = true;
    // The loop produced output: this turn is part of the conversation and
    // can no longer be rescued.
    app.handle_event(TuiEvent::Runtime(text("working")));
    // The turn end arrives (the engine returned a cancel) but the engine's
    // `RunCancelled` is nowhere — the exact delivery hole.
    app.handle_event(TuiEvent::TurnCancelled);
    let last = app.transcript.output.last().unwrap();
    assert!(
        last.text.contains("cancelled"),
        "the turn-final verdict reads cancelled, not done: {}",
        last.text
    );
    assert!(!app.running);
    // The late engine event only adds its marker row below the summary.
    app.handle_event(TuiEvent::Runtime(run_cancelled(None)));
    assert_eq!(
        app.transcript.output.last().unwrap().kind,
        LineKind::Cancelled
    );
}

/// Delivery order must not matter: when the engine's `RunCancelled` wins the
/// race the marker lands first, and the later `TurnCancelled` settle still
/// reads "cancelled" (its own `pending_cancel` stamp is a no-op on the
/// already-snapshotted summary).
#[test]
fn run_cancelled_first_then_the_settle_still_reads_cancelled() {
    let mut app = App::new();
    submit_rescuable(&mut app, "run the loop");
    app.running = true;
    app.handle_event(TuiEvent::Runtime(text("working")));
    app.handle_event(TuiEvent::Runtime(run_cancelled(None)));
    assert_eq!(
        app.transcript.output.last().unwrap().kind,
        LineKind::Cancelled
    );
    app.handle_event(TuiEvent::TurnCancelled);
    let last = app.transcript.output.last().unwrap();
    assert!(
        last.text.contains("cancelled"),
        "the verdict reads cancelled: {}",
        last.text
    );
    assert!(
        last.kind != LineKind::Cancelled,
        "the summary sits below the marker row"
    );
    assert!(!app.running);
}

/// A rescued turn settles quiet even when its turn end is a cancel: no
/// verdict stamp, and the late `RunCancelled` still finds its rescue gate
/// armed (the settle reads the flag, it never consumes it).
#[test]
fn turn_cancelled_keeps_a_rescued_turn_quiet() {
    let mut app = App::new();
    let before = app.transcript.output.len();
    submit_rescuable(&mut app, "typo message");
    app.running = true;
    assert!(app.take_cancel_rescue().is_some());
    app.handle_event(TuiEvent::TurnCancelled);
    assert_eq!(app.transcript.output.len(), before, "no verdict, no rows");
    assert!(!app.pending_cancel);
    app.handle_event(TuiEvent::Runtime(run_cancelled(None)));
    assert_eq!(app.transcript.output.len(), before, "still quiet");
    assert!(!app.pending_cancel);
}
