//! The TUI main loop: terminal setup, the event/command channels, and the
//! agent-side turn runner.
//!
//! Architecture (§9.3): the agent loop is untouched. A background task runs
//! turns and forwards `RuntimeEvent`s through an mpsc channel; the main loop
//! drains those events into [`App`] state, polls keyboard input, and redraws.
//! Input and events meet only through the channel — no shared mutable state.

mod agent_loop;
mod input;
mod notices;
mod pump;
mod resume_replay;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub(super) use {agent_loop::*, input::*, notices::*, pump::*, resume_replay::*};

use std::io;

use std::path::{Path, PathBuf};

use std::sync::Arc;

use std::time::{Duration, Instant};

use anyhow::Result;

use crossterm::{
    event::{
        DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyEventKind,
        KeyboardEnhancementFlags, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use phi_agent::{
    AgentResult, ChatMessage, ChildResultEvent, PhiAgent, RunOutcome, RuntimeEvent, SessionContext,
    SessionId, persist_window_messages, read_session_title, save_turn_log,
};

use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};

use tokio::sync::mpsc;

use super::app::{Action, App, TuiEvent};

use super::child_results::{ChildResultRoute, ChildResultRouter};

use super::frame_log::{FrameLogs, PerfRow};

use super::render;

use super::session_state::CurrentSession;

use super::shell::{
    CommandSpec, CopyTable, PromptSource, RouteOutcome, SessionBoot, ShellFlow, ShellHandles,
    ShellSpec, SkillBridge, TickFacts, TickOut, UpdateSlot, hook_guard, hook_guard_async,
    slash_picker_rows,
};

use super::task_layout;

#[cfg(unix)]
use super::term_guard;

// Signature types for the extracted loop fns.
use super::shell::{
    NoticeProbe, PanelTick, ReopenHook, SessionIo, SubmitHook, TipsSlot, TitleSlot,
};
use crate::approval::ApprovalItem;
use crate::interaction::{AskItem, PickItem};

/// A command from the TUI loop to the agent task.
enum Cmd {
    Run {
        /// The turn's input text.
        input: String,
        /// Machine-composed input (background wake, child batch): the turn
        /// runs the same, but the message is tagged synthetic so the rotation
        /// trail never mistakes it for the user's own words. A click answer
        /// is real user intent and stays `false`.
        synthetic: bool,
        /// Pasted screenshots riding this user message
        /// ([`crate::app::PendingImage`]) — attached to the user message, not
        /// judged here. Always empty for synthetic turns.
        images: Vec<crate::app::PendingImage>,
    },
    Quit,
}

/// Outcome of one TUI lifecycle (one session's worth of UI).
enum TuiOutcome {
    /// User pressed Ctrl+C / Ctrl+D twice.
    Quit,
    /// User selected a different session via `/resume`.
    SwitchSession(PathBuf),
}

/// Liveness tick: while a turn or a child wait is in flight, re-render at
/// least this often even with zero events, so the spinner/elapsed readout in
/// the status bar advance through silent stretches (a long LLM call emits no
/// events — the old purely event-driven loop froze the whole screen for its
/// full duration, session 20260904_e6612477, "frozen for a while"). Tick-only redraws are
/// NOT captured to frames.txt — an animated status bar would otherwise defeat
/// the flipbook's content dedup (~4 frames/s of nothing but spinner motion).
const UI_TICK: Duration = Duration::from_millis(250);

/// Opportunistic tips refresh re-check: the event loop compares this cadence
/// against `last_tips_check` (no timer task) and spawns one more background
/// fetch pass when it lapses. Cache and merge policy live behind the product's
/// fetch closure ([`crate::shell::TipsFetchFn`]); a fresh cache makes the pass
/// a no-op there and a failing endpoint is retried at this cadence rather
/// than every tick.
const TIPS_RECHECK: Duration = Duration::from_secs(300);

/// Spawn one background tips refresh pass through the product's fetch
/// closure, delivering `TuiEvent::TipsUpdated` only when it returns new
/// content. Failures are silent (debug log) — tips are decoration, never a
/// startup or runtime dependency.
fn spawn_tips_refresh(fetch: &crate::shell::TipsFetchFn, tx: mpsc::UnboundedSender<TuiEvent>) {
    let fetch = Arc::clone(fetch);
    let trace_tx = tx.clone();
    let fut: super::shell::TaskFuture = Box::pin(async move {
        if let Some(items) = fetch().await {
            let _ = tx.send(TuiEvent::TipsUpdated { items });
        }
    });
    spawn_traced("tips.fetch", fut, trace_tx);
}

/// Spawn a product background future under supervision (T12): a panic inside
/// a detached task would kill only that task and leave no trace, so the
/// future is joined by a watcher that reports the join error as a degradation
/// event — the run loop then logs it and marks the frame flipbook. The panic
/// itself never reaches the event loop.
fn spawn_traced(
    name: &'static str,
    fut: super::shell::TaskFuture,
    tx: mpsc::UnboundedSender<TuiEvent>,
) {
    let handle = tokio::spawn(fut);
    tokio::spawn(async move {
        if handle.await.is_err() {
            let _ = tx.send(TuiEvent::Degrade(name));
        }
    });
}

/// T12 triage: record a product-hook degradation — one tracing error plus a
/// mark in the frame flipbook (when the instrument trio is on) so post-hoc
/// review sees it next to the frames it affected. Silent degradations are
/// forbidden: this is their trace.
fn degrade(frame_logs: &mut Option<FrameLogs>, name: &str) {
    tracing::error!(hook = name, "product hook degraded to fallback");
    if let Some(logs) = frame_logs {
        logs.degrade(name);
    }
}

/// The `/resume` picker row — present only for session-based products (O6:
/// 不填即关; T13): no session slot, no row, no command.
fn resume_row(session_io: &Option<crate::shell::SessionIo>) -> Option<(String, String)> {
    session_io
        .as_ref()
        .map(|io| ("resume".to_string(), io.resume_help.clone()))
}

/// Copy `text` to the clipboard (if available), surfacing a status-bar notice.
fn copy_text(app: &mut App, clipboard: &mut Option<arboard::Clipboard>, text: String) {
    if text.is_empty() {
        app.set_notice("nothing to copy");
    } else {
        match clipboard.as_mut() {
            Some(cb) => match cb.set_text(text.clone()) {
                Ok(_) => app.set_notice(format!("📋 copied {} chars", text.chars().count())),
                Err(e) => app.set_notice(format!("copy failed: {e}")),
            },
            None => app.set_notice("clipboard unavailable"),
        }
    }
}

/// Run the interactive ratatui TUI until the user quits.
///
/// Takes ownership of the agent/session because the turn runner lives in a
/// spawned task for the whole lifetime of the TUI.
///
/// Wire-up signature (T2 seam cut): [`ShellSpec`] is the product's data
/// configuration, [`ShellHandles`] the runtime live objects (O1 two-stage).
/// The body destructures both back to the pre-cut local names — behavior is
/// unchanged by the seam.
pub async fn run(spec: ShellSpec, agent: Arc<PhiAgent>, handles: ShellHandles) -> Result<()> {
    let ShellSpec {
        data_dir: base_dir,
        paths,
        input_history,
        workspace,
        name,
        version,
        wordmark,
        theme,
        popup_style,
        keymap,
        copy,
        commands,
        title_gen,
        mut startup_notices,
        update,
        tips,
        mut notices,
        session_io,
        system_prompt,
        skills,
        mut tick,
        on_submit,
        on_reopen,
        is_consent,
        vision,
        exit_dump: exit_dump_mode,
    } = spec;
    let ShellHandles {
        session_state,
        bg_registry,
        approval_rx,
        pick_rx,
        approval_mode,
        log_sinks,
        session_boot,
        session_end,
    } = handles;
    let CurrentSession {
        session_id: session,
        ctx: session_ctx,
        resume_messages,
    } = session_state;
    // Keep the pre-cut `version: &str` shape for the body below.
    let version = version.as_str();

    // Session state (updated on each /resume switch).
    let mut session = session;
    let mut session_ctx = session_ctx;
    let mut resume_messages = resume_messages;

    // Terminal setup. The guard restores the terminal on any exit path.
    enable_raw_mode()?;

    // Panic hook: restore the terminal BEFORE the default hook prints, or the
    // panic text lands mid-frame in raw mode and smears across the status bar
    // and input box (observed: a tool panic on a tokio worker corrupted the
    // composer for the rest of the session). Installed once per process —
    // /resume re-enters this function and must not stack hooks.
    static PANIC_HOOK: std::sync::Once = std::sync::Once::new();
    PANIC_HOOK.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stderr(), LeaveAlternateScreen);
            default_hook(info);
        }));
    });

    let mut stdout = io::stdout();
    enter_terminal_mode(&mut stdout)?;
    let _guard = TerminalGuard;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Approval channel lives for the whole TUI lifetime (session-independent).
    let mut approval_rx = approval_rx;
    // So does the product pick channel (`/model` chooser).
    let mut pick_rx = pick_rx;

    // Whether ANY wheel/trackpad scroll event arrived during this whole run.
    // Session switches keep it: zero scrolls across a full session in Apple
    // Terminal is the fingerprint of "Allow Mouse Reporting" being off in
    // that window, reported at exit (see wheel_dead_notice).
    let mut saw_scroll = false;

    // ── Outer loop: each iteration = one session's TUI lifecycle ──
    loop {
        // Force a clean redraw after picker or session switch.
        terminal.clear()?;

        let session_dir = session_ctx.session_dir.clone();
        let session_id = session_ctx.session_id.clone();
        let log_path = session_ctx.log_path().display().to_string();

        // Session logs (frame capture, perf timing, composer state): one
        // instrument behind `ShellSpec.paths` (T11) — `None` means the
        // product turned the trio off. Each log owns its file handle +
        // dedup/throttle state; see frame_log.rs. Opened before session_boot
        // (T12) so boot degradations land in the flipbook; an open failure
        // degrades to the trio being off (warn + None), never aborts the run.
        let mut frame_logs = match paths.as_ref() {
            Some(p) => match FrameLogs::open(&session_dir, p) {
                Ok(logs) => Some(logs),
                Err(err) => {
                    tracing::warn!(%err, "frame logs failed to open; continuing without them");
                    None
                }
            },
            None => None,
        };

        // Per-session product boot (design §3): the product builds its session
        // tools (registered root-only like `add_tool`) and hands over the
        // queues the run loop serves — the `ask_user` picker queue and the
        // product panel's consent prompts. Re-armed every session iteration:
        // a `/resume` switch must not carry one session's queues into the
        // next. The product arms its own interaction slot inside the closure.
        // T12: a panicking product boot degrades to an empty boot (no tools,
        // no queues) — the session still opens, the degradation is traced.
        let (boot, boot_degraded) = hook_guard_async(
            "session_boot",
            {
                let session_boot = Arc::clone(&session_boot);
                let session_id = session_id.clone();
                let base_dir = base_dir.clone();
                move || (session_boot)(&session_id, &base_dir)
            },
            SessionBoot {
                tools: Vec::new(),
                ask_rx: None,
                consent_rx: None,
            },
        )
        .await;
        let SessionBoot {
            tools: boot_tools,
            mut ask_rx,
            mut consent_rx,
        } = boot;
        if let Some(name) = boot_degraded {
            degrade(&mut frame_logs, name);
        }
        {
            let registry = agent.runtime().tools_mut();
            for tool in boot_tools {
                registry.write().await.register_arc(tool);
            }
        }

        // Two channels: events flow agent → TUI, commands flow TUI → agent. Both
        // unbounded — the TUI drains eagerly and the agent must never be throttled
        // (a full bounded queue would drop events or stall `run_turn`).
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<TuiEvent>();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();

        // The `/` picker's data face: product skill rows, then the two
        // built-ins, then the product command rows that opt in (T5d). Help
        // wording is product copy throughout; typed-only commands
        // (`in_picker: false`) stay out of the list.
        let mut builtins = Vec::new();
        // Built-in command: /resume (not a skill, but triggered through the same
        // / popup). Session-based products only (see `resume_row`).
        if let Some(row) = resume_row(&session_io) {
            builtins.push(row);
        }
        // Built-in command: /upgrade (a local UI command, never reaches the LLM)
        if let Some(update) = &update {
            builtins.push(("upgrade".to_string(), update.help.clone()));
        }
        let skill_summaries = slash_picker_rows(
            skills
                .as_ref()
                .map(|b| {
                    // T12: a panicking summaries hook degrades to no skill rows.
                    let (rows, degraded) =
                        hook_guard("skills.summaries", || (b.summaries)(), Vec::new());
                    if let Some(hook) = degraded {
                        degrade(&mut frame_logs, hook);
                    }
                    rows
                })
                .unwrap_or_default(),
            builtins,
            &commands,
        );

        // Spawn the background update check (if enabled). The product's check
        // closure composes the full hint line; the shell only schedules and
        // displays it. Single attempt, silent on failure (the closure returns
        // `None` there). `--no-update-check` is already folded into
        // `auto_check` at construction — the manual /upgrade path runs `check`
        // unconditionally.
        if let Some(UpdateSlot { check, .. }) = update.as_ref().filter(|u| u.auto_check) {
            let check = Arc::clone(check);
            let update_tx = event_tx.clone();
            // T12: the call phase is product code — a panic there degrades to
            // "no check this boot"; the future is supervised so a panic
            // inside it is traced, not silently lost with its task.
            let (fut, degraded) = hook_guard(
                "update.check",
                || check(),
                Box::pin(std::future::ready(None)) as super::shell::TaskFuture<Option<String>>,
            );
            if let Some(hook) = degraded {
                let _ = update_tx.send(TuiEvent::Degrade(hook));
            } else {
                let trace_tx = update_tx.clone();
                spawn_traced(
                    "update.check",
                    Box::pin(async move {
                        if let Some(hint) = fut.await {
                            let _ = update_tx.send(TuiEvent::StatusHint(hint));
                        }
                    }),
                    trace_tx,
                );
            }
        }

        // Background tips refresh (if enabled). Startup renders the product's
        // initial rows immediately; this pass swaps in fresh content only when
        // the fetch says the version changed. Never blocks boot.
        if let Some(tips) = tips.as_ref() {
            spawn_tips_refresh(&tips.fetch, event_tx.clone());
        }

        // Start the child-result watcher (Phase 2): monitors the mailbox and
        // delivers each finished child's result as a ChildResultEvent. The loop
        // below decides per event: inject immediately (agent idle) or stash and
        // inject after the current turn ends (agent running).
        let (_watcher_handle, mut child_result_rx) =
            if let Some(ma_rt) = agent.multi_agent_runtime() {
                let (h, cr) = ma_rt.start_watcher();

                // Phase 5: forward registry lifecycle snapshots into the UI event
                // stream. The task panel reconciles against these facts (entries
                // appear at spawn, before the child's first tool call) instead of
                // inferring state purely from child runtime events.
                let mut life_rx = ma_rt.subscribe_lifecycle();
                let life_tx = event_tx.clone();
                tokio::spawn(async move {
                    while life_rx.changed().await.is_ok() {
                        let snap = life_rx.borrow_and_update().clone();
                        if life_tx.send(TuiEvent::Lifecycle(snap)).is_err() {
                            break; // UI loop gone
                        }
                    }
                });

                (Some(h), Some(cr))
            } else {
                (None, None)
            };

        let agent_task = tokio::spawn(agent_loop(
            agent.clone(),
            skills.clone(),
            system_prompt.clone(),
            copy.clone(),
            name.to_string(),
            session.clone(),
            session_ctx,
            event_tx.clone(),
            cmd_rx,
            commands.clone(),
        ));
        // Set once the agent task is observed dead (panic) — the failure is
        // surfaced in the transcript exactly once, not every tick.
        let mut agent_dead_announced = false;

        // Persistent event bridge: subscribe to the runtime's event bus each
        // session. The spawned task dies when event_tx is dropped (session ends).
        {
            let mut bus_rx = agent.runtime().subscribe_runtime_events();
            let bus_tx = event_tx.clone();
            tokio::spawn(async move {
                loop {
                    match bus_rx.recv().await {
                        Ok(ev) => {
                            if bus_tx.send(TuiEvent::Runtime(ev)).is_err() {
                                break; // UI loop gone
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(skipped = n, "UI event bus consumer lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        let mut app = App::new();
        app.approval_switch = Arc::clone(&approval_mode);
        app.set_theme(theme);
        app.set_popup_style(popup_style);
        app.set_keymap(keymap);
        app.set_copy(copy.clone());
        app.set_workspace_root(workspace.clone());
        // Input history (#7): fill the recall ring from the product's file
        // before the first key can reach it. Cloned: the loop rebuilds the
        // App per session restart and each build reloads.
        app.load_input_history(input_history.clone());
        app.set_skill_summaries(skill_summaries);
        app.set_background_registry(bg_registry.clone());
        if let Some(is_consent) = &is_consent {
            app.set_is_consent(Arc::clone(is_consent));
        }
        if let Some(vision) = &vision {
            app.set_vision(Arc::clone(vision));
        }
        if let Some(tips) = &tips {
            // The product's initial rows render from the first frame; the
            // refresh pass above only replaces them on a version change.
            app.set_tips(tips.initial.clone());
        }
        if let Some(wordmark) = &wordmark {
            // Divider width follows the startup window (see banner::build docs
            // for the accepted resize staleness). The rows come from the
            // product's wordmark generator (ShellSpec field, T5), fed this
            // iteration's session log path — the banner re-renders on every
            // session switch.
            let term_width = terminal
                .size()
                .map(|s| s.width as usize)
                .unwrap_or(80)
                .max(1);
            // T12: a panicking wordmark builder degrades to no banner rows —
            // the brand line below still names the product.
            let (rows, degraded) = hook_guard(
                "wordmark.build",
                || (wordmark.build.as_ref())(std::path::Path::new(&log_path), term_width),
                Vec::new(),
            );
            app.push_banner(rows);
            if let Some(hook) = degraded {
                degrade(&mut frame_logs, hook);
                let (fallback, _) = hook_guard(
                    "copy.brand_fallback",
                    || (app.copy.brand_fallback)(name, version),
                    String::new(),
                );
                if !fallback.is_empty() {
                    app.push_system(&fallback);
                }
            }
        } else {
            // No wordmark when the banner is off — the brand name has to
            // appear here or nowhere. The line is product copy
            // (CopyTable::brand_fallback, T5c). T12: a panicking formatter
            // degrades to no line, traced.
            let (fallback, degraded) = hook_guard(
                "copy.brand_fallback",
                || (app.copy.brand_fallback)(name, version),
                String::new(),
            );
            if let Some(hook) = degraded {
                degrade(&mut frame_logs, hook);
            }
            if !fallback.is_empty() {
                app.push_system(&fallback);
            }
        }

        // Clipboard for Ctrl+Y "copy last reply". Optional: on headless or
        // Linux/Wayland setups `arboard` may fail to open a clipboard; copy then
        // just reports "clipboard unavailable" instead of crashing.
        let mut clipboard = arboard::Clipboard::new().ok();

        // Boot-time notices (clamp / binary shadows / code-road degrade and
        // friends, aggregated product-side before run()). Visible on the
        // first frame, never silent; pushed exactly once (a session switch
        // does not replay it).
        for notice in std::mem::take(&mut startup_notices) {
            app.push_system(&format!("! {notice}"));
        }

        // Replay historical messages into the transcript so the user sees the
        // previous conversation (not just a blank screen) after --resume.
        if let Some(messages) = &resume_messages {
            // Archived windows first. `messages.jsonl` only ever holds the
            // live window, so without this a rotated session opens on its
            // recent tail and the whole past is invisible — which from the
            // outside is exactly "resume shows no history". Display only:
            // the model still resumes from the live window and recovers the
            // rest through the history.* tools.
            // T12: a panicking archive reader degrades to "no archive" — the
            // live window still replays; the degradation is traced. No session
            // slot = no archives either.
            let (archived, degraded) = match &session_io {
                Some(io) => hook_guard(
                    "session_io.archived",
                    || (io.archived)(&base_dir, &session_id),
                    Vec::new(),
                ),
                None => (Vec::new(), None),
            };
            if let Some(hook) = degraded {
                degrade(&mut frame_logs, hook);
            }
            // A restored window (or a rotation's user trail) sits in both
            // slices — show it once, from the archive, which is the fuller
            // copy.
            let fresh = drop_already_shown(&archived, messages);
            replay_messages_to_transcript(&mut app, &archived, fresh);
            if !messages.is_empty() || !archived.is_empty() {
                // Digest goes AFTER the replay block: the viewport follows the
                // bottom, so this lands on the visible last screen — right
                // where the user is looking, telling them the history above is
                // browsable with PgUp (half a page per press).
                let title = read_session_title(&session_dir);
                app.push_system(&resume_digest_line(&archived, fresh, title));
            }
        }

        // Terminal.app forwards wheel/trackpad events to the app only while
        // View → Allow Mouse Reporting is checked for the window; when off, the
        // swipe is swallowed by the terminal and scrolling silently dies. Give
        // Apple Terminal users the menu path plus the keyboard fallback
        // (fn+Up/Down = PgUp/PgDn) up front.
        if let Some(notice) = terminal_scroll_notice(std::env::var("TERM_PROGRAM").ok().as_deref())
        {
            app.push_system(&notice);
        }

        // Track user messages for title generation at the 10-message threshold.
        let mut user_msg_count: usize = 0;

        // `dirty` gates the offscreen snapshot: only re-render when state changed,
        // and then at most once per the capture interval.
        let mut dirty = true; // capture the initial screen
        // Last real render time — drives the UI_TICK liveness redraw.
        let mut last_draw = Instant::now();
        // Last terminal-geometry probe — drives the 1 Hz size self-heal below.
        let mut last_geom_check = Instant::now();
        // Last termios probe — drives the 1 Hz raw-mode self-heal below.
        #[cfg(unix)]
        let mut last_term_guard = Instant::now();
        // Last tips refresh re-check — drives the opportunistic pass below.
        let mut last_tips_check = Instant::now();
        // Last size we know the buffers were resized to. The 1 Hz probe below
        // compares the REAL terminal size against this; autoresize alone is
        // not enough because it resizes buffers without clearing the screen.
        let mut last_size = crossterm::terminal::size().unwrap_or((0, 0));

        // Child-result delivery (fan-in redesign). The watcher coordinates: a
        // Progress event is display-only, a Batch event wakes the parent. The
        // router decides per event — inject immediately when the agent is idle,
        // hold and flush as one batch right after the turn ends when it is
        // running — and the loop below only executes the side effects. Decision
        // logic lives in `child_results` (unit-tested); this loop must stay free
        // of timing policy.
        let mut child_results = ChildResultRouter::new();
        let mut outcome: Option<TuiOutcome> = None;
        while outcome.is_none() {
            pump(
                &mut app,
                &mut terminal,
                &mut frame_logs,
                &agent,
                &agent_task,
                &mut event_rx,
                &event_tx,
                &cmd_tx,
                &commands,
                &mut child_result_rx,
                &mut child_results,
                &mut ask_rx,
                &mut consent_rx,
                &mut approval_rx,
                &mut pick_rx,
                &mut notices,
                &mut tick,
                &tips,
                &update,
                &session_io,
                &on_submit,
                &on_reopen,
                &title_gen,
                &mut session,
                &session_id,
                &session_dir,
                &base_dir,
                &mut clipboard,
                &mut dirty,
                &mut last_draw,
                &mut last_geom_check,
                #[cfg(unix)]
                &mut last_term_guard,
                &mut last_tips_check,
                &mut last_size,
                &mut agent_dead_announced,
                &mut saw_scroll,
                &mut user_msg_count,
                &mut outcome,
            )
            .await?;
        }

        // ── End of inner event loop ──

        // Ensure the agent task is told to stop and has a chance to finish.
        let _ = cmd_tx.send(Cmd::Quit);
        let _ = agent_task.await;

        // Session teardown is product policy behind the end hook (design §3):
        // graceful panel close (open tabs land on the farewell page before the
        // server stops), unbinds, and any per-session tool-face reset. Covers
        // both outcomes: Quit, and a `/resume` switch aborting the old
        // session's side state before the next iteration boots its own.
        // T12: a panicking teardown degrades to "no teardown" — the session
        // still ends; the degradation is traced.
        let end_hook = Arc::clone(&session_end);
        let (_, end_degraded) = hook_guard_async("session_end", move || end_hook(), ()).await;
        if let Some(hook) = end_degraded {
            degrade(&mut frame_logs, hook);
        }

        tracing::info!("tui: agent task joined, flushing logs");
        if let Some(logs) = &mut frame_logs {
            logs.flush();
        }

        tracing::info!("tui: logs flushed, entering outcome match");
        match outcome.unwrap_or(TuiOutcome::Quit) {
            TuiOutcome::Quit => {
                // Write a pending-title marker on exit.  The title will be generated
                // asynchronously on next startup — zero blocking here. The
                // marker's layout under the sessions dir is product policy
                // (the write hook); the threshold and the existing-title
                // check are mechanism.
                if user_msg_count < 10
                    && read_session_title(&session_dir).is_none()
                    && let Some(title) = &title_gen
                {
                    let sessions_dir = session_dir.parent().unwrap();
                    // T12: a panicking marker writer degrades to "no marker" —
                    // next startup just misses the pending title.
                    let (_, marker_degraded) = hook_guard(
                        "title.write_marker",
                        || (title.write_marker)(sessions_dir, &session_dir),
                        (),
                    );
                    if let Some(hook) = marker_degraded {
                        degrade(&mut frame_logs, hook);
                    }
                }

                // Shutdown background task registry: cancel all running tasks and
                // SIGKILL all process groups. Must be explicit — executor tasks hold
                // Arc<Registry> so Drop won't fire.
                tracing::info!("tui: quit arm, shutting down bg registry");
                bg_registry.shutdown();
                tracing::info!("tui: bg registry shut down, dropping terminal guard");

                // Restore the terminal BEFORE printing so the diagnostic lands in
                // the normal screen, visible above the next shell prompt.
                drop(_guard);
                tracing::info!("tui: guard dropped, replaying session to restored terminal");
                // Replay the transcript now that the primary screen is back:
                // the alt-screen content is gone the instant we left it, and
                // nothing else keeps the conversation on screen. The rows land
                // in the shell's scrollback (Claude Code parity).
                // T12: exit-dump copy formatters are product code — a panic
                // there skips the replay (traced) instead of unwinding over
                // the already-restored terminal.
                let replayed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::exit_dump::replay_to_stdout(
                        exit_dump_mode,
                        &app.copy,
                        &crate::exit_dump::SessionFacts {
                            version,
                            session_id: &session_id,
                            session_dir: &session_dir,
                            turns: user_msg_count,
                            tokens_in: app.tokens_in,
                            tokens_out: app.tokens_out,
                        },
                        &app.transcript.output,
                        &app.sub_agent_transcripts,
                    )
                }));
                if replayed.is_err() {
                    degrade(&mut frame_logs, "exit_dump.replay");
                }
                tracing::info!("tui: session replayed, evaluating wheel diagnostic");
                if let Some(notice) = wheel_dead_notice(
                    std::env::var("TERM_PROGRAM").ok().as_deref(),
                    saw_scroll,
                    app.viewport.rendered_total > app.viewport.viewport_height,
                ) {
                    tracing::info!("wheel diagnostic: printing to restored terminal");
                    println!("{}", notice);
                } else {
                    tracing::info!(saw_scroll, "wheel diagnostic: not fired");
                }
                tracing::info!("tui: quit complete");

                return Ok(());
            }
            TuiOutcome::SwitchSession(picked_dir) => {
                // Heal before loading: a session that rotated and then died
                // may hold its conversation only in the history archive.
                let picked_id = picked_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                // The archive layout is product storage — healing is the
                // product's restore hook. T12: a panicking restore degrades to
                // "nothing healed" — the switch proceeds on whatever the
                // picked dir holds; the degradation is traced. No session slot
                // = nothing to heal (a switch cannot happen without one).
                if let Some(io) = &session_io {
                    let (_, restore_degraded) = hook_guard(
                        "session_io.restore",
                        || (io.restore)(&base_dir, &picked_id, &picked_dir),
                        false,
                    );
                    if let Some(hook) = restore_degraded {
                        degrade(&mut frame_logs, hook);
                    }
                }
                match agent.switch_to_session(&picked_dir, &base_dir).await {
                    Ok((new_id, messages, new_ctx)) => {
                        // Re-point the tracing file sink so logs follow the data
                        // directory. Without this, logs keep streaming into the
                        // launch session's dir while messages/turn logs/metrics go
                        // to the resumed session's dir (2026-09-22 bfef1017 vs
                        // 317d95e1 split — forensics had to correlate two dirs).
                        if new_ctx.log_path().display().to_string() != log_path {
                            match log_core::FileSink::new(new_ctx.log_path()).await {
                                Ok(sink) => {
                                    log_sinks.replace_sinks(vec![Box::new(sink)]).await;
                                    tracing::info!(
                                        path = %new_ctx.log_path().display(),
                                        "logging re-pointed to resumed session directory"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "failed to re-point logging after session switch; logs stay in the launch directory");
                                }
                            }
                        }
                        session = new_id;
                        session_ctx = new_ctx;
                        resume_messages = Some(messages);
                        continue; // Re-enter outer loop: fresh App + channels + agent_loop
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "session switch failed, shutting down");
                        bg_registry.shutdown();
                        // Out is out: same replay as the quit arm. Drop the
                        // guard first so the rows land on the primary screen.
                        drop(_guard);
                        // T12: same degrade as the quit arm — a panicking
                        // exit-dump formatter skips the replay, traced.
                        let replayed =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                crate::exit_dump::replay_to_stdout(
                                    exit_dump_mode,
                                    &app.copy,
                                    &crate::exit_dump::SessionFacts {
                                        version,
                                        session_id: &session_id,
                                        session_dir: &session_dir,
                                        turns: user_msg_count,
                                        tokens_in: app.tokens_in,
                                        tokens_out: app.tokens_out,
                                    },
                                    &app.transcript.output,
                                    &app.sub_agent_transcripts,
                                )
                            }));
                        if replayed.is_err() {
                            degrade(&mut frame_logs, "exit_dump.replay");
                        }
                        return Ok(());
                    }
                }
            }
        }
    } // end outer loop
}
