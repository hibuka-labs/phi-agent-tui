//! The TUI main loop: terminal setup, the event/command channels, and the
//! agent-side turn runner.
//!
//! Architecture (§9.3): the agent loop is untouched. A background task runs
//! turns and forwards `RuntimeEvent`s through an mpsc channel; the main loop
//! drains those events into [`App`] state, polls keyboard input, and redraws.
//! Input and events meet only through the channel — no shared mutable state.

use std::io;
use std::path::PathBuf;
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
    ChatMessage, ChildResultEvent, PhiAgent, RunOutcome, RuntimeEvent, SessionContext, SessionId,
    persist_window_messages, read_session_title, save_turn_log,
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
#[cfg(unix)]
use super::term_guard;

/// A command from the TUI loop to the agent task.
enum Cmd {
    Run(String),
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
        exit_dump: exit_dump_mode,
    } = spec;
    let ShellHandles {
        session_state,
        bg_registry,
        approval_rx,
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
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    // Kitty keyboard protocol: lets crossterm read the Command (Super) modifier
    // so Cmd+C can be bound to copy. Terminals that don't support it (e.g. the
    // macOS Terminal.app) ignore this and keep legacy key reporting.
    execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    )?;
    let _guard = TerminalGuard;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Approval channel lives for the whole TUI lifetime (session-independent).
    let mut approval_rx = approval_rx;

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
        app.set_skill_summaries(skill_summaries);
        app.set_background_registry(bg_registry.clone());
        if let Some(is_consent) = &is_consent {
            app.set_is_consent(Arc::clone(is_consent));
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
            let loop_start = Instant::now();

            // Drain any events queued since the last frame into state.
            // `running_before` captures the root-turn falling edge (1C row
            // 15's boundary) before anything below can start another turn —
            // child inject / bg wake / click drain all re-raise `running` at
            // their send sites.
            let running_before = app.running;
            while let Ok(ev) = event_rx.try_recv() {
                // Thought deltas only feed the thinking panel, which animates on
                // the 250 ms UI_TICK — redrawing per delta bought nothing but a
                // wall of churn; ≤4 fps reads calmer. Prose deltas still redraw
                // at event rate.
                let thought_only = matches!(
                    &ev,
                    super::app::TuiEvent::Runtime(RuntimeEvent::ThoughtDelta { .. })
                );
                // T12: a degradation reported from the agent task lands in the
                // flipbook here — that task has no frame_logs handle. It is
                // logged by `degrade`, not re-handled by the app.
                if let super::app::TuiEvent::Degrade(hook) = ev {
                    degrade(&mut frame_logs, hook);
                    dirty = true;
                    continue;
                }
                app.handle_event(ev);
                if !thought_only {
                    dirty = true;
                }
            }
            let turn_fell = running_before && !app.running;

            // Turn-end flush (Claude Code: queued messages go out
            // automatically, in typed order — never a queue that silently
            // rots). Anything the engine never drained goes out now as its own
            // turn: steering messages merged as one, held `/` commands one at
            // a time. The `sync` first echoes anything that WAS delivered in
            // the turn's last stretch, so the flush owns only what never
            // reached the model. Fires on any idle frame with a non-empty
            // queue, not just the falling edge — a queue can outlive its run
            // (cancel races), and the flush must not depend on catching one.
            if !app.running && (!app.queued_steer.is_empty() || !app.queued_cmds.is_empty()) {
                app.sync_steer_queue(agent.runtime().steering_snapshot());
                let leftover = agent.runtime().take_steering();
                app.queued_steer.clear();
                let held_cmds = std::mem::take(&mut app.queued_cmds);
                if !leftover.is_empty() {
                    let joined = leftover.join("\n");
                    app.begin_user_turn(&joined);
                    if cmd_tx.send(Cmd::Run(joined)).is_ok() {
                        app.running = true;
                    } else {
                        // Channel closed — undo the commit (same rule as the
                        // Submit send site) so no phantom turn is shown.
                        app.settle_status_from_inflight();
                    }
                }
                for cmd in held_cmds {
                    app.begin_user_turn(&cmd);
                    if cmd_tx.send(Cmd::Run(cmd)).is_ok() {
                        app.running = true;
                    } else {
                        app.settle_status_from_inflight();
                    }
                }
                dirty = true;
            }

            // Poll the product's notice probe (window rotations, futility
            // brake and friends: runtime announcements whose wording and
            // de-dup policy are product-owned). Each returned line is pushed
            // once; the probe keeps its own "already announced" state.
            // T12: a panicking probe degrades to no lines and is disabled —
            // a per-loop hook that panicked once is broken, and re-entering it
            // every tick would flood the degradation trace.
            if let Some(probe) = notices.clone() {
                let (lines, degraded) = hook_guard("notices.probe", move || probe(), Vec::new());
                if let Some(hook) = degraded {
                    degrade(&mut frame_logs, hook);
                    notices = None;
                }
                for line in lines {
                    app.push_system(&line);
                    dirty = true;
                }
            }

            // Drain child-result events from the watcher task.
            if let Some(rx) = child_result_rx.as_mut() {
                while let Ok(cr) = rx.try_recv() {
                    // Task-panel bookkeeping: a finished child flips its panel
                    // entry to done and refreshes the Waiting count. Must happen
                    // before routing so the status strip is current this frame.
                    if let ChildResultEvent::Progress {
                        agent_path, status, ..
                    } = &cr
                        && status != "running"
                    {
                        app.mark_sub_agent_finished(agent_path);
                    }
                    let route = child_results.on_event(app.running, cr);
                    tracing::info!(
                        running = app.running,
                        route = ?std::mem::discriminant(&route),
                        "tui: child_result event routed"
                    );
                    match route {
                        ChildResultRoute::Notice { notice } => app.push_system(&notice),
                        ChildResultRoute::Hold { notice } => app.set_notice(notice),
                        ChildResultRoute::Inject { notice, input } => {
                            // A batch means every child returned — settle the panel
                            // before the synthetic run starts.
                            app.mark_all_sub_agents_finished();
                            app.push_system(&notice);
                            if cmd_tx.send(Cmd::Run(input)).is_ok() {
                                app.running = true;
                            }
                        }
                    }
                    dirty = true;
                }
            }

            // The turn just ended and results arrived while it was running —
            // inject them now as one synthetic run.
            if !app.running
                && let Some(ChildResultRoute::Inject { notice, input }) =
                    child_results.flush_when_idle()
            {
                tracing::info!("tui: flush_when_idle -> injecting batch");
                app.mark_all_sub_agents_finished();
                app.push_system(&notice);
                if cmd_tx.send(Cmd::Run(input)).is_ok() {
                    app.running = true;
                }
                dirty = true;
            }

            // Background-task auto-wake (the bg sibling of the fan-in batch):
            // every background shell task has ended, nothing else is in flight,
            // and at least one done/timed-out/errored outcome is unreported —
            // start a synthetic run telling the agent to fetch each output via
            // `task_output` and report it. The delivery gates (hold while the
            // agent is mid-turn / children run / other tasks run, the 15s
            // aggregation quiet window, Cancelled suppressed, report-once)
            // live in `bg_wake::take_bg_wake`; this loop only executes the
            // side effects.
            if let Some((notice, input)) = app.take_bg_wake(Instant::now()) {
                tracing::info!("tui: bg_wake -> injecting background task report");
                app.push_system(&notice);
                if cmd_tx.send(Cmd::Run(input)).is_ok() {
                    app.running = true;
                }
                dirty = true;
            }

            // Product panel tick (design §3): canvas subscriptions, lifecycle
            // polls, click drains, root-turn edges, consent — all product state
            // behind one closure; the shell only executes the outcome (notes to
            // the transcript, the status-band face, a synthetic turn). The
            // closure owns its async work (spawning where it must await) and
            // its de-dup; it is called once per loop iteration with this
            // tick's facts. T16: a synthetic turn is honored only when no turn
            // is active — never injected into a running one.
            let composer_holds = !app.composer.is_empty();
            let busy = app.running || matches!(app.status, super::app::AgentStatus::Running { .. });
            if let Some(tick_fn) = tick.clone() {
                // T12: a panicking product tick degrades to a no-op tick —
                // never an unwind through the loop — and is disabled: a
                // per-loop hook that panicked once is broken, and re-entering
                // it every tick would flood the degradation trace.
                let (out, degraded) = hook_guard(
                    "tick",
                    move || {
                        tick_fn(&TickFacts {
                            running: busy,
                            composer_holds,
                            turn_fell,
                            now: Instant::now(),
                        })
                    },
                    TickOut {
                        notes: Vec::new(),
                        synthetic: None,
                        status: None,
                        dirty: false,
                    },
                );
                if let Some(name) = degraded {
                    degrade(&mut frame_logs, name);
                    tick = None;
                }
                for note in out.notes {
                    app.push_system(&note);
                    dirty = true;
                }
                // The face is assigned unconditionally: `None` clears it, so a
                // panel can retract its status line without a sentinel.
                if app.panel_status != out.status {
                    app.panel_status = out.status;
                    dirty = true;
                }
                if let Some(syn) = out.synthetic
                    && !app.running
                {
                    tracing::info!("tui: panel synthetic -> user answer");
                    // Same commit rule as the Submit send site: user-turn state
                    // lands here, and a click answer counts toward the title
                    // threshold (`Synthetic::user`).
                    app.begin_user_turn(&syn.text);
                    if cmd_tx.send(Cmd::Run(syn.text)).is_ok() {
                        app.running = true;
                        if syn.user {
                            user_msg_count += 1;
                        }
                    } else {
                        app.settle_status_from_inflight();
                    }
                    dirty = true;
                }
                dirty |= out.dirty;
            }

            // Cleanup completed sub-agents (auto-remove after 3 seconds)
            if app.cleanup_completed_agents() {
                dirty = true;
            }

            // Reconcile background shell tasks from the registry (tick polling).
            if app.reconcile_background_tasks() {
                dirty = true;
            }

            // Reconcile the steering mirror: what the engine drained at the
            // last tool boundary is now the model's input, so it leaves the
            // pending strip and lands in the transcript (one echo per message,
            // at delivery — not at queue time).
            if app.sync_steer_queue(agent.runtime().steering_snapshot()) {
                dirty = true;
            }

            // Detect a dead agent task. `agent_task` is only awaited at exit,
            // so a panic that escaped tool-level isolation would otherwise
            // leave the spinner turning forever with the user typing into a
            // dead session. `outcome.is_none()` skips the normal quit path,
            // where Quit is sent to the agent right before the UI exits.
            if outcome.is_none() && !agent_dead_announced && agent_task.is_finished() {
                agent_dead_announced = true;
                tracing::error!("tui: agent task finished unexpectedly (panic or early exit)");
                app.push_system(
                    "! Agent runtime terminated unexpectedly - input is no longer \
                     processed. Quit (Ctrl+C) and resume the session; see session.log \
                     for the panic details.",
                );
                dirty = true;
            }

            // Drain queued approval requests into the popup queue (one popup at a
            // time — the front of the queue is what gets rendered). The product
            // panel's consent prompts ride their own channel (SessionBoot)
            // into the same queue: they are prompts like any other, only their
            // key legend differs.
            let had_approval = app.has_pending_approval();
            while let Ok(item) = approval_rx.try_recv() {
                app.approval_queue.push_back(item);
                dirty = true;
            }
            if let Some(consent_rx) = consent_rx.as_mut() {
                while let Ok(item) = consent_rx.try_recv() {
                    app.approval_queue.push_back(item);
                    dirty = true;
                }
            }
            // `ask_user` (concept routing): a question parked in the popup
            // queue — the tool is parked on its oneshot until the user picks
            // or skips, and the transcript stays visible behind the window.
            // No alternate-screen bounce any more: the question is a popup in
            // this frame, like an approval prompt.
            let had_ask = app.has_pending_ask();
            if let Some(ask_rx) = ask_rx.as_mut() {
                while let Ok(item) = ask_rx.try_recv() {
                    app.ask_queue.push_back(item);
                    dirty = true;
                }
            }
            // BUG 4: a popup that just opened must never see keys typed before it
            // appeared — those were aimed at the composer, not at a prompt nobody
            // had seen yet (`/plugin`'s trailing n once decided Deny this way).
            // Drop whatever is still buffered and keep Resizes (they self-heal in
            // the 1 Hz geometry probe anyway if they were lost). Keys that survive
            // into the popup can at worst select an option — bare Enter is still
            // required to commit.
            let popup_opened = (!had_approval && app.has_pending_approval())
                || (!had_ask && app.has_pending_ask());
            if popup_opened {
                let mut dropped_keys = 0usize;
                while crossterm::event::poll(Duration::ZERO)? {
                    match crossterm::event::read()? {
                        Event::Resize(w, h) => {
                            tracing::info!(w, h, "tui: resize during popup-open input drain");
                            terminal.resize(Rect::new(0, 0, w, h))?;
                            terminal.clear()?;
                            last_size = (w, h);
                            dirty = true;
                        }
                        Event::Key(_) | Event::Paste(_) => dropped_keys += 1,
                        _ => {}
                    }
                }
                if dropped_keys > 0 {
                    tracing::info!(dropped_keys, "tui: drained pre-popup input at popup open");
                }
            }

            // Poll input with a short timeout so the frame rate stays bounded even
            // while a long turn streams events. 2 ms keeps scroll responsive (~500 Hz
            // poll rate, gated by draw cost); the old 16 ms added perceptible jank
            // on mouse-wheel scrolling because each event blocked for a full frame.
            //
            // Drain ALL pending crossterm events in one go — `read()` returns one
            // event at a time, so a fast scroll-wheel burst leaves the rest queued.
            // Without this loop, each leftover event waits an extra 2 ms poll cycle,
            // creating visible stutter on track-pad / inertial scrolling.
            let mut crossterm_count: u32 = 0;
            let mut has_scroll = false;
            let mut has_key = false;
            let mut has_mouse = false;
            let mut has_paste = false;
            let mut has_resize = false;
            while crossterm::event::poll(Duration::ZERO)? {
                let event = crossterm::event::read()?;
                crossterm_count += 1;
                match event {
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        dirty = true;
                        has_key = true;
                        if let Some(action) =
                            app.handle_key_event(key.code, key.modifiers, key.kind)
                        {
                            match action {
                                Action::Submit(text) => {
                                    // Intercept built-in commands before sending to agent.
                                    // `/resume` exists only for session-based products
                                    // (O6: 不填即关) — no slot means no command.
                                    if text.trim() == "/resume"
                                        && let Some(session_io) = &session_io
                                    {
                                        let picked = super::picker::show_picker_from_tui(
                                            &base_dir,
                                            Some(session_id.as_str()),
                                            app.theme(),
                                            session_io,
                                        )?;
                                        // The picker bounces LeaveAlternateScreen
                                        // / EnterAlternateScreen, so we come back
                                        // to a freshly blanked alternate screen.
                                        // ratatui's draw() diffs against its cached
                                        // previous buffer and re-emits only changed
                                        // cells — with the screen blank but the cache
                                        // intact that means the banner and borders
                                        // are never repainted (the "display got
                                        // messed up" after a
                                        // cancelled picker). Clearing invalidates the
                                        // cache and forces a full repaint.
                                        terminal.clear()?;
                                        if let Some(picked_dir) = picked {
                                            outcome = Some(TuiOutcome::SwitchSession(picked_dir));
                                        }
                                        // Don't send to agent, don't count as user message.
                                        break;
                                    }
                                    // /upgrade: manual update check + display
                                    // result. The command exists only when the
                                    // product ships an update slot (its `/`
                                    // row is advertised on the same
                                    // condition); the check closure carries
                                    // the wording and the "already up to date"
                                    // silence.
                                    if text.trim() == "/upgrade"
                                        && let Some(update) = &update
                                    {
                                        let check = Arc::clone(&update.check);
                                        let update_tx = event_tx.clone();
                                        // T12: same degrade as the auto check —
                                        // guarded call phase, supervised future.
                                        let (fut, degraded) = hook_guard(
                                            "update.check",
                                            || check(),
                                            Box::pin(std::future::ready(None))
                                                as super::shell::TaskFuture<Option<String>>,
                                        );
                                        if let Some(hook) = degraded {
                                            let _ = update_tx.send(TuiEvent::Degrade(hook));
                                        } else {
                                            let hint_tx = update_tx.clone();
                                            spawn_traced(
                                                "update.check",
                                                Box::pin(async move {
                                                    if let Some(hint) = fut.await {
                                                        let _ = hint_tx
                                                            .send(TuiEvent::StatusHint(hint));
                                                    }
                                                }),
                                                update_tx,
                                            );
                                        }
                                        break;
                                    }
                                    // Enter-boundary product rewrite (design
                                    // §3): pending panel picks merge behind
                                    // the typed text as one user message
                                    // (typed first, blank line, then the pick
                                    // lines); `None` = submit unchanged. The
                                    // product also settles its waiting summary
                                    // inside the hook.
                                    let text = match &on_submit {
                                        Some(hook) => {
                                            // T12: panic degrades to "submit
                                            // unchanged", traced to the log.
                                            let (rewritten, degraded) =
                                                hook_guard("on_submit", || hook(&text), None);
                                            if let Some(name) = degraded {
                                                degrade(&mut frame_logs, name);
                                            }
                                            rewritten.unwrap_or(text)
                                        }
                                        None => text,
                                    };
                                    // Commit the turn's UI state at the send
                                    // site — key handling only yields
                                    // Action::Submit, and this branch is past
                                    // every interceptor (/resume, /upgrade),
                                    // so the echo/clock/phase can no longer
                                    // outlive a submit that never queued a run.
                                    app.begin_user_turn(&text);
                                    if cmd_tx.send(Cmd::Run(text)).is_ok() {
                                        // Root turn queued. Synthetic turns set the
                                        // same flag at their send sites — `running`
                                        // means "a turn is in flight or queued", and
                                        // it is cleared only by settle_after_turn.
                                        app.running = true;
                                    } else {
                                        // Channel closed — the agent loop is gone, so
                                        // nothing will ever settle this turn. Undo the
                                        // commit rather than leave a phantom
                                        // "thinking" stretch with no run behind it
                                        // (the same failure mode the commit moved
                                        // here to prevent). The dead-agent detector
                                        // reports the underlying loss.
                                        app.settle_status_from_inflight();
                                    }
                                    user_msg_count += 1;
                                    if user_msg_count == 10 {
                                        // Fire-and-forget: the shell owns the
                                        // milestone trigger (the 10th user
                                        // turn); what "generate a title" means
                                        // — model, wording, persistence — is
                                        // product strategy behind the
                                        // TitleGenerator fn field (T5d).
                                        if let Some(title) = &title_gen {
                                            let msgs = agent
                                                .runtime()
                                                .get_messages(&session)
                                                .await
                                                .unwrap_or_default();
                                            // T12: the generator's call phase is
                                            // product code — a panic there
                                            // degrades to "no title this
                                            // milestone" instead of unwinding
                                            // through the loop. The spawned
                                            // future's own failures live and die
                                            // inside its task.
                                            let (fut, degraded) = hook_guard(
                                                "title.generate",
                                                || (title.generate)(msgs, session_dir.clone()),
                                                Box::pin(std::future::ready(()))
                                                    as super::shell::TaskFuture,
                                            );
                                            if let Some(hook) = degraded {
                                                degrade(&mut frame_logs, hook);
                                            } else {
                                                // Supervised spawn: a panic
                                                // inside the generator's future
                                                // is traced, not lost with a
                                                // detached task.
                                                spawn_traced(
                                                    "title.generate",
                                                    fut,
                                                    event_tx.clone(),
                                                );
                                            }
                                        }
                                    }
                                }
                                Action::Approve(decision) => app.approve_front(decision),
                                Action::CopyLastReply => {
                                    let text = app.last_reply_text();
                                    copy_text(&mut app, &mut clipboard, text);
                                }
                                Action::CopySelection => {
                                    let text = app.selection_text();
                                    copy_text(&mut app, &mut clipboard, text);
                                    app.clear_selection();
                                    tracing::info!("selection cleared after copy");
                                }
                                Action::Redraw => {
                                    // Same repair as the Resize path: clear
                                    // invalidates ratatui's diff cache so every
                                    // cell goes back out, overwriting whatever
                                    // the terminal clipped or left stale.
                                    tracing::info!("ctrl+l: forced full repaint");
                                    terminal.clear()?;
                                    dirty = true;
                                    app.set_notice("screen redrawn");
                                }
                                Action::CanvasReopen => {
                                    // The panel-reopen key (its binding is
                                    // keymap data): the product hook rebinds
                                    // its server — async because a rebind may
                                    // mint a new URL — and settles its own
                                    // consent and notes (a failure note rides
                                    // the next tick's `TickOut::notes`).
                                    // `None` = the key does nothing. Awaited
                                    // inline so the redraw sees the result.
                                    if let Some(reopen) = &on_reopen {
                                        // T12: either phase (the call or its
                                        // future) may panic — degrade, trace,
                                        // redraw.
                                        let (_, degraded) = hook_guard_async(
                                            "on_reopen",
                                            {
                                                let reopen = Arc::clone(reopen);
                                                move || reopen()
                                            },
                                            (),
                                        )
                                        .await;
                                        if let Some(name) = degraded {
                                            degrade(&mut frame_logs, name);
                                        }
                                        dirty = true;
                                    }
                                }
                                Action::Cancel => {
                                    agent.cancel();
                                    // Dismiss the popup(s) and drop any in-flight
                                    // request not yet drained (the cancelled handler
                                    // stops sending once its token fires). A consent
                                    // prompt dropped mid-air is answered as "nothing
                                    // was opened" — safe direction (no browser).
                                    app.clear_approvals();
                                    while approval_rx.try_recv().is_ok() {}
                                    if let Some(consent_rx) = consent_rx.as_mut() {
                                        while consent_rx.try_recv().is_ok() {}
                                    }
                                    // A question nobody is waiting on any more:
                                    // dropping the item answers the tool right
                                    // away instead of parking a picker over a
                                    // cancelled turn.
                                    app.clear_asks();
                                    if let Some(ask_rx) = ask_rx.as_mut() {
                                        while ask_rx.try_recv().is_ok() {}
                                    }
                                }
                                Action::Steer(text) => {
                                    if text.trim_start().starts_with('/') {
                                        // A raw `/foo` steering message is just
                                        // text to the model — hold the command
                                        // and run it as its own turn at the end.
                                        app.queued_cmds.push(text);
                                        app.set_notice(
                                            "Command queued - runs after the current turn",
                                        );
                                    } else {
                                        agent.runtime().steer(text.clone());
                                        app.queued_steer.push(text);
                                        app.set_notice("Queued - delivered at the next step (Up takes it back)");
                                    }
                                    dirty = true;
                                }
                                Action::TakeBackQueued => {
                                    // Undelivered steering messages return to
                                    // the composer (queue order, above the
                                    // draft). Delivered ones already echoed
                                    // and are gone from the engine queue, so
                                    // `take_steering` can never resurrect a
                                    // message the model has seen.
                                    let msgs = agent.runtime().take_steering();
                                    app.take_back_into_composer(msgs);
                                    dirty = true;
                                }
                                Action::ForceSend(text) => {
                                    if !text.trim().is_empty() {
                                        if text.trim_start().starts_with('/') {
                                            app.queued_cmds.push(text);
                                        } else {
                                            agent.runtime().steer(text.clone());
                                            app.queued_steer.push(text);
                                        }
                                    }
                                    // Interrupt the turn; when it settles the
                                    // idle-frame flush sends the queue right
                                    // away (simplified `chat:sendNow`).
                                    agent.cancel();
                                    app.clear_approvals();
                                    while approval_rx.try_recv().is_ok() {}
                                    if let Some(consent_rx) = consent_rx.as_mut() {
                                        while consent_rx.try_recv().is_ok() {}
                                    }
                                    app.clear_asks();
                                    if let Some(ask_rx) = ask_rx.as_mut() {
                                        while ask_rx.try_recv().is_ok() {}
                                    }
                                    dirty = true;
                                }
                                Action::Quit => {
                                    agent.cancel();
                                    let _ = cmd_tx.send(Cmd::Quit);
                                    outcome = Some(TuiOutcome::Quit);
                                }
                            }
                        }
                    }
                    Event::Mouse(MouseEvent {
                        kind: MouseEventKind::ScrollUp,
                        ..
                    }) => {
                        has_scroll = true;
                        saw_scroll = true;
                        if app.scroll_wheel_up() {
                            dirty = true;
                        }
                    }
                    Event::Mouse(MouseEvent {
                        kind: MouseEventKind::ScrollDown,
                        ..
                    }) => {
                        has_scroll = true;
                        saw_scroll = true;
                        if app.scroll_wheel_down() {
                            dirty = true;
                        }
                    }
                    Event::Mouse(MouseEvent {
                        kind, column, row, ..
                    }) => {
                        dirty = true;
                        has_mouse = true;
                        // The copy-menu hit-test needs the terminal size to locate
                        // the popup exactly where it was drawn.
                        let size = terminal.size()?;
                        if let Some(action) =
                            app.handle_mouse(kind, column, row, size.width, size.height)
                            && let Action::CopySelection = action
                        {
                            let text = app.selection_text();
                            copy_text(&mut app, &mut clipboard, text);
                            app.clear_selection();
                        }
                    }
                    Event::Paste(text) => {
                        dirty = true;
                        has_paste = true;
                        app.paste(&text);
                    }
                    Event::Resize(w, h) => {
                        // Repaint unconditionally: mid-drag SIGWINCHs coalesce
                        // into a single event whose size may equal the buffer
                        // size, yet the terminal has already CLIPPED cells off
                        // the right edge. Diff rendering can't repair cells it
                        // believes unchanged - resize + clear forces every cell
                        // back out, or a 1-col "notch" stays on the composer's
                        // right border until the next effective resize.
                        // Logged: same-size events are the fingerprint of that
                        // coalescing (session 20261014_25c28bee — a size pulse
                        // garbled the screen and this path repaired it).
                        tracing::info!(
                            w,
                            h,
                            last_w = last_size.0,
                            last_h = last_size.1,
                            "tui: resize event -> clear+repaint"
                        );
                        has_resize = true;
                        terminal.resize(Rect::new(0, 0, w, h))?;
                        terminal.clear()?;
                        last_size = (w, h);
                        dirty = true;
                    }
                    _ => {}
                }
            }
            // Build a compact event-type tag for the log (e.g. "S" / "K" / "SM" / "SK";
            // "R" = resize, "P" = paste — they used to be invisible, which made
            // repair-path events indistinguishable from ghosts).
            let mut event_types = String::new();
            if has_scroll {
                event_types.push('S');
            }
            if has_key {
                event_types.push('K');
            }
            if has_mouse {
                event_types.push('M');
            }
            if has_paste {
                event_types.push('P');
            }
            if has_resize {
                event_types.push('R');
            }

            // If no events arrived, sleep briefly to avoid busy-spinning.
            let slept = !dirty;
            if slept {
                std::thread::sleep(Duration::from_millis(2));
            }

            // Geometry self-heal (1 Hz): a SIGWINCH burst can coalesce into NO
            // Resize event at all (size ends where it started) or the event
            // races the drain loop, while ratatui's per-draw autoresize only
            // resizes buffers - it never clears cells the terminal already
            // clipped mid-drag. Probe the real size once a second; on drift,
            // resize + clear forces a full repaint and heals the notch.
            if last_geom_check.elapsed() >= Duration::from_secs(1) {
                last_geom_check = Instant::now();
                let probed = crossterm::terminal::size().unwrap_or(last_size);
                if probed != last_size {
                    tracing::info!(?last_size, ?probed, "tui: geometry self-heal resize");
                    last_size = probed;
                    terminal.resize(Rect::new(0, 0, probed.0, probed.1))?;
                    terminal.clear()?;
                    dirty = true;
                }
            }

            // Terminal-integrity self-heal (1 Hz): an external clobber — the
            // BUG 7 incident had an MCP child's npm/node flipping the tty back
            // to cooked — kills input without any app-side signal: keystrokes
            // sit in the kernel line buffer and crossterm never sees them.
            // Probe the real termios once a second; on drift, force raw back
            // (disable→enable — crossterm's enable is a no-op while it still
            // believes raw is on) and clear, so the next draw also covers the
            // garbage the child scribbled onto the alt-screen.
            #[cfg(unix)]
            if last_term_guard.elapsed() >= term_guard::CHECK_INTERVAL {
                last_term_guard = Instant::now();
                if !term_guard::raw_mode_intact() {
                    tracing::warn!("tui: termios drifted out of raw mode; re-asserting");
                    term_guard::reassert_raw_mode();
                    terminal.clear()?;
                    dirty = true;
                }
            }

            // Opportunistic tips refresh (see TIPS_RECHECK): a session that
            // outlives the refresh interval gets another background pass;
            // the cache timestamp keeps a fresh one a no-op.
            if let Some(tips) = &tips
                && last_tips_check.elapsed() >= TIPS_RECHECK
            {
                last_tips_check = Instant::now();
                spawn_tips_refresh(&tips.fetch, event_tx.clone());
            }

            // Liveness tick: active (Running/Waiting) with no events this
            // iteration → redraw so spinner/elapsed advance. Tick-only frames are
            // drawn but not captured (see UI_TICK).
            let mut tick_fired = false;
            if !dirty && app.is_active() && last_draw.elapsed() >= UI_TICK {
                dirty = true;
                tick_fired = true;
            }

            // Only redraw when state actually changed (dirty flag). With a 2 ms
            // poll timeout the idle loop would otherwise burn ~500 useless draws/s.
            let mut draw_elapsed = Duration::ZERO;
            if dirty {
                last_draw = Instant::now();
                let draw_start = Instant::now();
                // T12: render-path product hooks (CopyTable formatters, the
                // consent classifier) panic out of product code — skip the
                // frame and trace it, never unwind through the loop. An io
                // error from the terminal itself still propagates (`?`).
                let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // `.map(|_| ())` so the frame's borrow of `terminal` does
                    // not escape the closure.
                    terminal.draw(|f| render::draw(f, &mut app)).map(|_| ())
                }));
                match drawn {
                    Ok(result) => {
                        result?;
                    }
                    Err(_) => degrade(&mut frame_logs, "terminal.draw"),
                }
                draw_elapsed = draw_start.elapsed();

                // Log composer state after each draw (only when state changes).
                if let Some(logs) = &mut frame_logs {
                    let size = terminal.size()?;
                    logs.composer
                        .record(&app, logs.frames.frame_id(), (size.width, size.height));
                }
            }

            // Record the frame if it changed since the last one (dedup keeps the
            // flipbook small — steady states and per-token deltas collapse away).
            // Gated on `dirty` + the capture interval so the offscreen render only
            // runs after a real change, at most ~10×/s.
            let mut capture_elapsed = Duration::ZERO;
            if dirty && let Some(logs) = &mut frame_logs {
                let size = terminal.size()?;
                // T12: the offscreen snapshot walks the same render path —
                // a panicking product formatter skips this capture, traced.
                let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    logs.frames
                        .capture(!tick_fired, &mut app, size.width, size.height)
                }));
                match captured {
                    Ok(spent) => capture_elapsed = spent,
                    Err(_) => logs.degrade("frame.capture"),
                }
            }
            let loop_elapsed = loop_start.elapsed();
            // Log every frame to perf.log (CSV) for post-hoc analysis.
            // Note: dirty is logged BEFORE reset so the column reflects whether
            // a draw/capture actually happened this iteration.
            if let Some(logs) = &mut frame_logs {
                logs.perf.record(&PerfRow {
                    frame_id: logs.frames.frame_id(),
                    draw_ms: draw_elapsed.as_millis(),
                    capture_ms: capture_elapsed.as_millis(),
                    loop_ms: loop_elapsed.as_millis(),
                    dirty,
                    scroll_offset: app.viewport.scroll_offset,
                    follow_bottom: app.viewport.follow_bottom,
                    output_lines: app.transcript.len(),
                    crossterm_events: crossterm_count,
                    slept,
                    event_types: &event_types,
                });
            }
            // Reset dirty after draw + optional capture so idle loops skip both.
            dirty = false;
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

/// The agent-side task: run turns on command, forward events to the TUI, and
/// persist per-turn JSONL logs exactly like the REPL path does.
// Turn-side wire-up: a working subset of `run()`'s unpacked handles; the allow covers this long list.
#[allow(clippy::too_many_arguments)]
async fn agent_loop(
    agent: Arc<PhiAgent>,
    skills: Option<SkillBridge>,
    system_prompt: PromptSource,
    copy: CopyTable,
    product_name: String,
    session: SessionId,
    session_ctx: SessionContext,
    event_tx: mpsc::UnboundedSender<TuiEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    commands: Vec<CommandSpec>,
) {
    // Resume turn numbering from any turns already logged for this session.
    let mut turn_number = session_ctx.last_turn_number();

    // Initialize phi-telemetry for per-turn token usage tracking.
    let mut telemetry = phi_telemetry::init_telemetry(
        agent.runtime(),
        session_ctx.session_id.clone(),
        product_name,
        agent.config.model.clone(),
    );

    // Live token readout for the status bar: forward every model call's real
    // usage as it settles (the same numbers phi-telemetry accumulates for
    // session_metrics.json). Fires per model call, so the readout grows
    // through the tool loop instead of only at turn end. Root agent only —
    // sub-agent tokens stay in their own sessions' metrics.
    {
        let usage_tx = event_tx.clone();
        agent.runtime().on_turn_end(move |ctx| {
            if let Some(usage) = &ctx.usage {
                let _ = usage_tx.send(TuiEvent::Usage {
                    input_tokens: usage.prompt_tokens.unwrap_or(0).into(),
                    output_tokens: usage.completion_tokens.unwrap_or(0).into(),
                });
            }
        });
    }

    // Session-scope skills (skill-lifetime v3): the product reloads its
    // active list and re-bakes it into the system prompt on entry. Covers
    // both process resume (fresh agent, restored history) and session
    // switches (fresh agent_loop).
    //
    // The bake base is the PRISTINE build-time prompt captured once from the
    // runtime config — that string already carries everything the builder
    // injected (catalog, token-budget suffix, CLAUDE.md, memory index).
    // A `None` back means nothing is active: leave the prompt alone.
    let bake_base: String = agent.system_prompt().await.unwrap_or_else(|| {
        // T12: a panicking prompt fallback degrades to an empty base —
        // rebake then leaves the prompt alone; traced via the flipbook
        // channel (this task has no frame_logs handle).
        let (base, degraded) = hook_guard(
            "system_prompt.fallback",
            || (system_prompt.fallback)(),
            String::new(),
        );
        if let Some(hook) = degraded {
            let _ = event_tx.send(TuiEvent::Degrade(hook));
        }
        base
    });
    if let Some(skills) = &skills {
        // T12: a panicking rebake degrades to "nothing active" — the prompt
        // is left alone; traced.
        let (rebaked, degraded) = hook_guard("skills.rebake", || (skills.rebake)(&bake_base), None);
        if let Some(hook) = degraded {
            let _ = event_tx.send(TuiEvent::Degrade(hook));
        }
        if let Some(prompt) = rebaked
            && let Err(e) = agent.set_system_prompt(&session, prompt).await
        {
            tracing::warn!(error = %e, "failed to re-bake active skills on resume");
        }
    }

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Cmd::Quit => {
                // Finalize telemetry and persist session metrics.
                telemetry.shutdown().await;
                let metrics = telemetry.session.read().await;
                let mut metrics = metrics.clone();
                metrics.finalize(phi_telemetry::SessionOutcome::Completed);
                let _ = phi_telemetry::save_metrics(&metrics, &session_ctx.session_dir);
                break;
            }
            Cmd::Run(input) => {
                // Product command face (T5d): a `/name` submit is offered to
                // the product's command table first — local semantics,
                // settled via SystemMessage (no turn_number), never a model
                // turn. Commands serialize on this loop, so the socket board
                // sees one at a time; a plugin install blocks here until the
                // human answers its full-facts confirm popup.
                if let Some(spec) = commands.iter().find(|c| c.matches(&input)) {
                    // T12: a panicking command action degrades to
                    // `ShellFlow::Unhandled` — the input falls through to skill
                    // routing and a plain turn, exactly like a declined
                    // command; traced via the flipbook channel.
                    let (flow, degraded) = hook_guard_async(
                        "spec.action",
                        {
                            let action = Arc::clone(&spec.action);
                            let input = input.clone();
                            move || action(input)
                        },
                        ShellFlow::Unhandled,
                    )
                    .await;
                    if let Some(hook) = degraded {
                        let _ = event_tx.send(TuiEvent::Degrade(hook));
                    }
                    if let ShellFlow::Answer(text) = flow {
                        let _ = event_tx.send(TuiEvent::SystemMessage(text));
                        continue;
                    }
                }
                // No name match, or `ShellFlow::Unhandled` (the name matched
                // but the command declined the input): fall through to skill
                // routing and a plain turn, exactly like a non-command input.

                turn_number += 1;
                let turn_events: std::sync::Arc<std::sync::Mutex<Vec<RuntimeEvent>>> =
                    std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

                // Skill routing (skill-lifetime v3): the product resolves the
                // slash command and declares what kind of turn follows
                // ([`RouteOutcome`]); the shell dispatches on the outcome.
                // Session-scope turns submit the RAW command: `$ARGUMENTS`
                // reach the model through it, and the body is already baked
                // into the prompt the product hands back. The turn log always
                // records the raw input below.
                let original_input = input.clone();
                let routed = match &skills {
                    Some(bridge) => {
                        // T12: a panicking router degrades to "plain turn" —
                        // the input goes to the model raw; traced.
                        let (outcome, degraded) = hook_guard(
                            "skills.route",
                            || (bridge.route)(&input),
                            RouteOutcome::Turn {
                                input: input.clone(),
                            },
                        );
                        if let Some(hook) = degraded {
                            let _ = event_tx.send(TuiEvent::Degrade(hook));
                        }
                        outcome
                    }
                    None => RouteOutcome::Turn { input },
                };

                // Pre-turn telemetry custom (the product's slash/skill usage
                // snapshot) must land BEFORE run_turn so the on_turn_end hook
                // (which fires inside run_turn) sees it. Model-triggered
                // events happen DURING the turn, after the hook fires — they
                // only appear in session-level custom (post-turn below).
                if let Some(bridge) = &skills {
                    // T12: a panicking custom snapshot degrades to "none".
                    let (custom, degraded) =
                        hook_guard("skills.turn_custom", || (bridge.turn_custom)(), None);
                    if let Some(hook) = degraded {
                        let _ = event_tx.send(TuiEvent::Degrade(hook));
                    }
                    if let Some(custom) = custom {
                        telemetry.set_turn_custom(custom);
                    }
                }

                let turn_events_clone = turn_events.clone();
                let result = match routed {
                    // Turn-scope skills submit the resolved body as an
                    // *ephemeral* user message: the LLM sees it for this turn
                    // only, then the engine strips it from memory and
                    // persistence at turn end. The turn leaves NO trace in
                    // history — neither body nor command (resume replay shows
                    // this turn's answer without a user line). The raw command
                    // lives only in the turn log below.
                    RouteOutcome::Ephemeral { input: body } => {
                        agent
                            .run_turn_ephemeral_input(session.clone(), &body, move |ev| {
                                // Persistence only — the UI is fed by the persistent
                                // bus subscription (see the bridge in `run`). Sending
                                // here too would double-deliver every event.
                                turn_events_clone.lock().unwrap().push(ev);
                                Ok(())
                            })
                            .await
                    }
                    RouteOutcome::SessionBaked {
                        input: body,
                        prompt,
                        skill,
                        warnings,
                    } => {
                        // The product composed the full prompt (pristine base +
                        // active skills); the shell only bakes it. A failure is
                        // a mid-turn warning: the run continues, so this must
                        // not settle the turn (TurnError does).
                        if let Err(e) = agent.set_system_prompt(&session, prompt).await {
                            tracing::warn!(
                                error = %e,
                                "failed to bake active skills into system prompt"
                            );
                            // T12: the warning wording is product copy — a
                            // panicking formatter degrades to no user line
                            // (the failure is already traced above).
                            let (warning, degraded) = hook_guard(
                                "copy.skill_bake_failed",
                                || (copy.skill_bake_failed)(&skill, &e.to_string()),
                                String::new(),
                            );
                            if let Some(hook) = degraded {
                                let _ = event_tx.send(TuiEvent::Degrade(hook));
                            }
                            if !warning.is_empty() {
                                let _ = event_tx.send(TuiEvent::Warning(warning));
                            }
                        }
                        // Product-composed warnings from the route step (e.g.
                        // the active list did not survive resume) surface
                        // before the turn runs.
                        for warning in warnings {
                            let _ = event_tx.send(TuiEvent::Warning(warning));
                        }
                        agent
                            .run_turn(session.clone(), &body, move |ev| {
                                turn_events_clone.lock().unwrap().push(ev);
                                Ok(())
                            })
                            .await
                    }
                    RouteOutcome::Turn { input: body } => {
                        agent
                            .run_turn(session.clone(), &body, move |ev| {
                                turn_events_clone.lock().unwrap().push(ev);
                                Ok(())
                            })
                            .await
                    }
                };

                // Persist regardless of success (matches the REPL behavior).
                // The log records the original command, never the skill body.
                let turn_events_vec = turn_events.lock().unwrap().clone();
                if let Err(e) =
                    save_turn_log(&session_ctx, turn_number, &turn_events_vec, &original_input)
                {
                    tracing::warn!(error = %e, "failed to save turn log");
                }

                // Snapshot current window messages for resume support.
                if let Ok(msgs) = agent.runtime().get_messages(&session).await
                    && let Err(e) = persist_window_messages(&session_ctx.session_dir, &msgs)
                {
                    tracing::warn!(error = %e, "failed to persist window messages");
                }

                // Post-turn telemetry: model-triggered skill events (recorded
                // during the turn) can't go into turn.custom (the on_turn_end
                // hook already fired), so they only appear in session-level
                // custom — the product snapshots (and clears) its per-turn
                // state inside the closure.
                if let Some(bridge) = &skills {
                    telemetry.set_session_custom((bridge.session_custom)());
                }
                // Yield so the observer processes SetSessionCustom before we
                // read the session for the per-turn save. Without this, the
                // save races with the observer and reads stale custom data.
                tokio::task::yield_now().await;

                let metrics = telemetry.session.read().await;
                let _ = phi_telemetry::save_metrics(&metrics, &session_ctx.session_dir);

                match result {
                    Ok(RunOutcome::Failed { error }) => {
                        let _ = event_tx.send(TuiEvent::TurnError(error));
                    }
                    Ok(RunOutcome::MaxTurnsExceeded { turns }) => {
                        let _ = event_tx
                            .send(TuiEvent::TurnError(format!("max turns ({turns}) exceeded")));
                    }
                    Ok(_) => {
                        let _ = event_tx.send(TuiEvent::TurnDone);
                    }
                    Err(e) => {
                        let _ = event_tx.send(TuiEvent::TurnError(format!("{e}")));
                    }
                }
            }
        }
    }
}

/// Restores the terminal (raw mode + alternate screen) when dropped.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            PopKeyboardEnhancementFlags,
            LeaveAlternateScreen
        );
    }
}

/// One-line startup notice for Apple Terminal users. Terminal.app forwards
/// wheel/trackpad events to the app only while View → Allow Mouse Reporting
/// is checked for that window (the macOS default is ON — an unchecked window
/// is almost always a prior ⌘R/“refresh” toggle). When it is off, the swipe
/// is swallowed by the terminal itself and scrolling silently dies.
/// `term_program` is the value of `$TERM_PROGRAM`; only `Apple_Terminal`
/// gets the notice.
fn terminal_scroll_notice(term_program: Option<&str>) -> Option<String> {
    if term_program != Some("Apple_Terminal") {
        return None;
    }
    Some(
        "Note: wheel/trackpad scroll needs View > Allow Mouse Reporting - fn+Up/fn+Down page history".to_string(),
    )
}

/// Exit-time diagnostic. Printed to the restored terminal when an entire run
/// in Apple Terminal never received a single wheel event even though the
/// transcript was taller than one screen — the exact fingerprint of Allow
/// Mouse Reporting being off in that window. Turns the previously silent
/// failure into an actionable message right after the session ends.
fn wheel_dead_notice(
    term_program: Option<&str>,
    saw_scroll: bool,
    scrollable: bool,
) -> Option<String> {
    if term_program != Some("Apple_Terminal") || saw_scroll || !scrollable {
        return None;
    }
    Some(
        "Note: no wheel events were received this run. If trackpad scrolling does nothing, enable View > Allow Mouse Reporting in your terminal and retry; fn+Up/fn+Down always work".to_string(),
    )
}

/// One-line digest pushed after the replayed history. The resume viewport
/// lands on the tail of the conversation, so without this the user has no
/// signal that the full history is loaded (or that PgUp reaches it).
/// Digest of what a resume restored: archived windows + the live window.
///
/// Both slices count — a rotated session's history is split across them, and
/// a digest that only mentioned the live window would under-report the
/// conversation the user just scrolled into.
fn resume_digest_line(
    archived: &[ChatMessage],
    current: &[ChatMessage],
    title: Option<String>,
) -> String {
    let messages: Vec<&ChatMessage> = archived.iter().chain(current.iter()).collect();
    let turns = messages
        .iter()
        .filter(|m| {
            matches!(
                m,
                ChatMessage::User {
                    ephemeral: false,
                    ..
                }
            )
        })
        .count();
    // Leading space so a titled digest reads `resumed session "foo"` and an
    // untitled one still reads `resumed session` with no dangling gap.
    let label = title.map(|t| format!(" \"{t}\"")).unwrap_or_default();
    let turn_word = if turns == 1 { "turn" } else { "turns" };
    let msg_word = if messages.len() == 1 {
        "message"
    } else {
        "messages"
    };
    format!(
        "↩ resumed session{label}: {turns} {turn_word}, {} {msg_word}. PgUp scrolls history, send a message to continue",
        messages.len()
    )
}

/// Drop the live-window head that already appears in the archive.
///
/// A restored window (or a rotation's user trail) sits in both slices — show
/// it once, from the archive, which is the fuller copy. Pure display de-dup
/// (the model still resumes from the live window as-is).
fn drop_already_shown<'a>(archived: &[ChatMessage], live: &'a [ChatMessage]) -> &'a [ChatMessage] {
    let seen: std::collections::HashSet<_> = archived.iter().filter_map(replay_key).collect();
    let mut start = 0;
    for msg in live {
        match replay_key(msg) {
            Some(key) if seen.contains(&key) => start += 1,
            _ => break,
        }
    }
    &live[start..]
}

/// Identity of a message for replay de-duplication.
fn replay_key(msg: &ChatMessage) -> Option<(u8, String, String)> {
    match msg {
        ChatMessage::User { content, .. } => Some((0, String::new(), content.clone())),
        ChatMessage::Assistant {
            content: Some(c), ..
        } if !c.is_empty() => Some((1, String::new(), c.clone())),
        ChatMessage::Tool { name, content, .. } => {
            Some((2, name.clone().unwrap_or_default(), content.clone()))
        }
        _ => None,
    }
}

/// Replay historical `ChatMessage`s into the TUI transcript so the user sees
/// the previous conversation after `--resume`.
///
/// `archived` are the rotated-out windows from the history store and
/// `current` is the live window from `messages.jsonl`. Archived first, in
/// chronological order, with a divider between the two only when both are
/// present.
fn replay_messages_to_transcript(
    app: &mut super::app::App,
    archived: &[ChatMessage],
    current: &[ChatMessage],
) {
    if archived.is_empty() && current.is_empty() {
        return;
    }

    app.push_system("── resumed conversation ──");

    for msg in archived {
        push_replayed_message(app, msg);
    }
    if !archived.is_empty() && !current.is_empty() {
        app.push_system("── current window ──");
    }
    for msg in current {
        push_replayed_message(app, msg);
    }

    app.push_system("── end of resumed conversation ──");
}

/// Push one historical message into the transcript as display lines.
fn push_replayed_message(app: &mut super::app::App, msg: &ChatMessage) {
    use phi_tui::lines::{LineKind, OutputLine};

    match msg {
        ChatMessage::User { content, .. } => {
            app.transcript.push_user(content);
        }
        ChatMessage::Assistant {
            content,
            tool_calls,
            ..
        } => {
            // Push text content (if any).
            // ONE raw OutputLine for the whole message: Normal prose is
            // wrapped at display time (markdown + `wrap_line`), and
            // `rewrap_output` keeps Normal lines whole for exactly that
            // reason. Pre-wrapping here froze the resume-time width into
            // the transcript, so a resized terminal kept stale breaks.
            if let Some(text) = content
                && !text.is_empty()
            {
                let text = text.clone();
                app.transcript.push(OutputLine {
                    original: Some(text.clone()),
                    text,
                    kind: LineKind::Normal,
                    spans: None,
                    detail: None,
                    tool_state: None,
                });
            }
            // Push tool call markers.
            if let Some(tcs) = tool_calls {
                for tc in tcs {
                    app.transcript.push(OutputLine {
                        text: format!("⚙ {}", tc.name),
                        kind: LineKind::Tool,
                        spans: None,
                        original: None,
                        detail: None,
                        tool_state: None,
                    });
                }
            }
        }
        ChatMessage::Tool { name, content, .. } => {
            let label = name.as_deref().unwrap_or("tool");
            let preview = content.lines().next().unwrap_or(content);
            // `elide`, not a byte slice: `&preview[..120]` panics mid-UTF-8
            // on non-ASCII tool output (any CJK repo), and the budget is
            // display columns so the row cannot overflow the pane.
            let display = format!(
                "↻ {}: {}",
                label,
                phi_tui::wrap::elide(preview, 120, phi_tui::wrap::Elide::Head)
            );
            app.transcript.push(OutputLine {
                text: display,
                kind: LineKind::ToolResult,
                spans: None,
                original: None,
                detail: None,
                tool_state: None,
            });
        }
        _ => {} // System / Custom — already filtered, skip.
    }
}

#[cfg(test)]
mod resume_digest_tests {
    use super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage::user(text)
    }

    fn assistant(text: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: Some(text.into()),
            reasoning_content: None,
            thinking_signature: None,
            tool_calls: None,
        }
    }

    #[test]
    fn digest_counts_turns_and_messages_with_title() {
        let msgs = vec![
            user("你好"),
            assistant("你好！"),
            ChatMessage::Tool {
                tool_call_id: "tc1".into(),
                name: Some("read".into()),
                content: "x".into(),
                images: Vec::new(),
            },
            user("继续"),
            assistant("好的"),
        ];
        let line = resume_digest_line(&[], &msgs, Some("探究工程".into()));
        assert!(line.contains("resumed session \"探究工程\""), "{line}");
        assert!(line.contains("2 turns"), "{line}");
        assert!(line.contains("5 messages"), "{line}");
        assert!(line.contains("PgUp"), "{line}");
    }

    #[test]
    fn digest_omits_title_when_absent() {
        let msgs = vec![user("hi"), assistant("hello")];
        let line = resume_digest_line(&[], &msgs, None);
        assert!(!line.contains('"'), "{line}");
        assert!(line.contains("1 turn, 2 messages"), "{line}");
    }

    /// Ephemeral user messages (skill bodies) never reach the persisted
    /// history, but if one ever did leak into a restored list it must not
    /// inflate the turn count the user is told about.
    #[test]
    fn digest_does_not_count_ephemeral_as_turn() {
        let msgs = vec![ChatMessage::user_ephemeral("skill body"), assistant("ok")];
        let line = resume_digest_line(&[], &msgs, None);
        assert!(line.contains("0 turns, 2 messages"), "{line}");
    }

    /// Terminal.app needs View → Allow Mouse Reporting before wheel events
    /// reach the app; users get an actionable notice instead of a dead
    /// trackpad. Other terminals (iTerm2, Ghostty, ...) are untouched.
    #[test]
    fn scroll_notice_targets_apple_terminal_only() {
        let notice = terminal_scroll_notice(Some("Apple_Terminal")).expect("notice expected");
        assert!(notice.contains("Allow Mouse Reporting"), "{notice}");
        assert!(notice.contains("fn+Up"), "{notice}");
        assert_eq!(terminal_scroll_notice(Some("iTerm.app")), None);
        assert_eq!(terminal_scroll_notice(Some("ghostty")), None);
        assert_eq!(terminal_scroll_notice(None), None);
    }

    /// The exit diagnostic fires only for the full failure fingerprint in
    /// Apple Terminal: zero wheel events all run, and there was more history
    /// than one screen. Any wheel event, another terminal, or a short
    /// transcript keeps the exit output clean.
    #[test]
    fn wheel_dead_notice_needs_apple_terminal_zero_scroll_and_history() {
        let notice =
            wheel_dead_notice(Some("Apple_Terminal"), false, true).expect("notice expected");
        assert!(notice.contains("no wheel events were received"), "{notice}");
        assert!(notice.contains("Allow Mouse Reporting"), "{notice}");
        // Saw a wheel event: reporting works, stay quiet.
        assert_eq!(wheel_dead_notice(Some("Apple_Terminal"), true, true), None);
        // Nothing to scroll: the message would be noise.
        assert_eq!(
            wheel_dead_notice(Some("Apple_Terminal"), false, false),
            None
        );
        // Other terminals handle wheel natively or via their own settings.
        assert_eq!(wheel_dead_notice(Some("iTerm.app"), false, true), None);
        assert_eq!(wheel_dead_notice(None, false, true), None);
    }
}

/// Replay must follow the Normal-prose contract: one raw `OutputLine`, wrapped
/// at display time. Pre-wrapping froze the resume-time width into the
/// transcript — a later resize kept the stale breaks, because
/// `rewrap_output` deliberately leaves Normal lines whole.
#[cfg(test)]
mod replay_tests {
    use super::*;
    use crate::app::App;
    use phi_tui::lines::LineKind;

    fn assistant(text: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: Some(text.into()),
            reasoning_content: None,
            thinking_signature: None,
            tool_calls: None,
        }
    }

    #[test]
    fn replay_keeps_prose_as_one_raw_line() {
        let mut app = App::new();
        let long = "The policy lives in the wake module and batches completions behind a quiet window so a burst of finished tasks costs one notification turn, not ten of them.";
        let msgs = vec![assistant(long)];
        replay_messages_to_transcript(&mut app, &[], &msgs);
        let normals: Vec<_> = app
            .transcript
            .output
            .iter()
            .filter(|l| l.kind == LineKind::Normal)
            .collect();
        assert_eq!(normals.len(), 1, "prose must stay one logical line");
        assert_eq!(normals[0].text, long);
        assert_eq!(normals[0].original.as_deref(), Some(long));
    }

    /// The tool preview used to be a raw byte slice at 120, which panics
    /// mid-UTF-8 on non-ASCII output (any CJK repo) during resume.
    #[test]
    fn replay_tool_preview_never_panics_on_multibyte() {
        let mut app = App::new();
        let msgs = vec![ChatMessage::Tool {
            tool_call_id: "tc1".into(),
            name: Some("bash".into()),
            content: "中".repeat(200), // 600 bytes — past the old cut
            images: Vec::new(),
        }];
        replay_messages_to_transcript(&mut app, &[], &msgs);
        let preview = app
            .transcript
            .output
            .iter()
            .find(|l| l.kind == LineKind::ToolResult)
            .expect("tool result line");
        assert!(preview.text.contains("bash"), "{}", preview.text);
        assert!(preview.text.len() < 300, "preview elided: {}", preview.text);
    }

    /// A rotated session splits its history across the archive and the live
    /// window. Resume used to replay only the live window, so the past was
    /// invisible — which from the outside is "resume shows no history".
    #[test]
    fn replay_shows_archived_windows_before_the_live_window() {
        let mut app = App::new();
        let archived = vec![ChatMessage::user("from the archived window")];
        let current = vec![assistant("from the live window")];
        replay_messages_to_transcript(&mut app, &archived, &current);

        let text: Vec<&str> = app
            .transcript
            .output
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        let archived_at = text
            .iter()
            .position(|t| t.contains("from the archived window"))
            .expect("archived line");
        let current_at = text
            .iter()
            .position(|t| t.contains("from the live window"))
            .expect("live line");
        assert!(
            archived_at < current_at,
            "archive is older, so it must render first"
        );
        assert!(
            text.iter().any(|t| t.contains("current window")),
            "a divider must separate the two when both are present: {text:?}"
        );
    }

    #[test]
    fn replay_omits_divider_when_only_one_source_has_content() {
        let mut app = App::new();
        replay_messages_to_transcript(&mut app, &[], &[assistant("only the live window")]);
        let text: Vec<&str> = app
            .transcript
            .output
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        assert!(
            !text.iter().any(|t| t.contains("current window")),
            "{text:?}"
        );
    }

    #[test]
    fn resume_digest_counts_archived_and_live_messages() {
        let archived = vec![
            ChatMessage::user("older question"),
            assistant("older answer"),
        ];
        let current = vec![ChatMessage::user("latest question")];
        let line = resume_digest_line(&archived, &current, None);
        assert!(line.contains("2 turns"), "{line}");
        assert!(line.contains("3 messages"), "{line}");
    }
}

#[cfg(test)]
mod session_slot_tests {
    use super::*;
    use crate::shell::SessionIo;
    use std::sync::Arc;

    fn session_io(help: &str) -> SessionIo {
        SessionIo {
            enumerate: Arc::new(|_, _| Vec::new()),
            archived: Arc::new(|_, _| Vec::new()),
            restore: Arc::new(|_, _, _| true),
            resume_help: help.to_string(),
        }
    }

    #[test]
    fn resume_row_follows_the_session_slot() {
        // O6 (T13): 不填即关 — no session slot, no /resume row; the slot's
        // help text is product copy that rides the row.
        assert!(resume_row(&None).is_none());
        let row = resume_row(&Some(session_io("Switch to another session")));
        assert_eq!(
            row,
            Some((
                "resume".to_string(),
                "Switch to another session".to_string()
            ))
        );
    }
}
