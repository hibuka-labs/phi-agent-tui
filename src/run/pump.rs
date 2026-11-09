//! One event-loop iteration: drain, serve ticks, heal, redraw, capture.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn pump(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    frame_logs: &mut Option<FrameLogs>,
    agent: &Arc<PhiAgent>,
    agent_task: &tokio::task::JoinHandle<()>,
    event_rx: &mut mpsc::UnboundedReceiver<TuiEvent>,
    event_tx: &mpsc::UnboundedSender<TuiEvent>,
    cmd_tx: &mpsc::UnboundedSender<Cmd>,
    commands: &[CommandSpec],
    child_result_rx: &mut Option<mpsc::UnboundedReceiver<ChildResultEvent>>,
    child_results: &mut ChildResultRouter,
    ask_rx: &mut Option<mpsc::UnboundedReceiver<AskItem>>,
    consent_rx: &mut Option<mpsc::UnboundedReceiver<ApprovalItem>>,
    approval_rx: &mut mpsc::UnboundedReceiver<ApprovalItem>,
    pick_rx: &mut mpsc::UnboundedReceiver<PickItem>,
    notices: &mut Option<NoticeProbe>,
    tick: &mut Option<PanelTick>,
    tips: &Option<TipsSlot>,
    update: &Option<UpdateSlot>,
    session_io: &Option<SessionIo>,
    on_submit: &Option<SubmitHook>,
    on_reopen: &Option<ReopenHook>,
    title_gen: &Option<TitleSlot>,
    session: &mut SessionId,
    session_id: &str,
    session_dir: &Path,
    base_dir: &Path,
    clipboard: &mut Option<arboard::Clipboard>,
    dirty: &mut bool,
    last_draw: &mut Instant,
    last_geom_check: &mut Instant,
    #[cfg(unix)] last_term_guard: &mut Instant,
    last_tips_check: &mut Instant,
    last_size: &mut (u16, u16),
    agent_dead_announced: &mut bool,
    saw_scroll: &mut bool,
    user_msg_count: &mut usize,
    outcome: &mut Option<TuiOutcome>,
) -> Result<()> {
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
            crate::app::TuiEvent::Runtime(RuntimeEvent::ThoughtDelta { .. })
        );
        // T12: a degradation reported from the agent task lands in the
        // flipbook here — that task has no frame_logs handle. It is
        // logged by `degrade`, not re-handled by the app.
        if let crate::app::TuiEvent::Degrade(hook) = ev {
            degrade(frame_logs, hook);
            *dirty = true;
            continue;
        }
        app.handle_event(ev);
        if !thought_only {
            *dirty = true;
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
    //
    // Two hard rules, both learned from a burst that replayed two
    // queued `/model` commands back-to-back and flip-flopped the real
    // model switch each one performs: (1) never fire while a modal
    // owns input — a pick/approval/ask is the user's turn to speak;
    // (2) exactly one run per pass — the rest stay queued and ride
    // later idle frames, so held commands can never stack into an
    // ambush. The queue is drained one settle at a time, not rotted.
    if !app.running
        && (!app.queued_steer.is_empty() || !app.queued_cmds.is_empty())
        && !app.has_pending_pick()
        && !app.has_pending_ask()
        && !app.has_pending_approval()
    {
        app.sync_steer_queue(agent.runtime().steering_snapshot());
        let leftover = agent.runtime().take_steering();
        app.queued_steer.clear();
        if let Some(input) = app.take_next_queued_run(leftover) {
            app.begin_user_turn(&input);
            if cmd_tx
                .send(Cmd::Run {
                    input,
                    synthetic: false,
                    images: Vec::new(),
                })
                .is_ok()
            {
                app.running = true;
            } else {
                // Channel closed — undo the commit (same rule as the
                // Submit send site) so no phantom turn is shown.
                app.settle_status_from_inflight();
            }
        }
        *dirty = true;
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
            degrade(frame_logs, hook);
            *notices = None;
        }
        for line in lines {
            app.push_system(&line);
            *dirty = true;
        }
    }

    // Drain child-result events from the watcher task.
    if let Some(rx) = child_result_rx.as_mut() {
        while let Ok(cr) = rx.try_recv() {
            // Task-panel bookkeeping: the wire carries each child's
            // outcome kind (ok/error/closed) — finalize the row (ledger
            // marker + counts) and refresh the Waiting count. Must
            // happen before routing so the status strip is current
            // this frame, and before the Inject route's catch-all
            // sweep can launder an unreported kind.
            match &cr {
                ChildResultEvent::Progress {
                    agent_path,
                    status,
                    summary,
                } => {
                    if let Some(kind) = task_layout::wire_status(status, summary.as_deref()) {
                        app.mark_sub_agent_finished(agent_path, kind);
                    }
                }
                ChildResultEvent::Batch { reports } => {
                    // Backstop per report: the Progress events
                    // already finalized these rows; finalize is
                    // idempotent, so no double ledger lines.
                    for r in reports {
                        if let Some(kind) = task_layout::wire_status(&r.status, r.result.as_deref())
                        {
                            app.mark_sub_agent_finished(&r.agent_path, kind);
                        }
                    }
                }
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
                    if cmd_tx
                        .send(Cmd::Run {
                            input,
                            synthetic: true,
                            images: Vec::new(),
                        })
                        .is_ok()
                    {
                        app.running = true;
                    }
                }
            }
            *dirty = true;
        }
    }

    // The turn just ended and results arrived while it was running —
    // inject them now as one synthetic run.
    if !app.running
        && let Some(ChildResultRoute::Inject { notice, input }) = child_results.flush_when_idle()
    {
        tracing::info!("tui: flush_when_idle -> injecting batch");
        app.mark_all_sub_agents_finished();
        app.push_system(&notice);
        if cmd_tx
            .send(Cmd::Run {
                input,
                synthetic: true,
                images: Vec::new(),
            })
            .is_ok()
        {
            app.running = true;
        }
        *dirty = true;
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
        if cmd_tx
            .send(Cmd::Run {
                input,
                synthetic: true,
                images: Vec::new(),
            })
            .is_ok()
        {
            app.running = true;
        }
        *dirty = true;
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
    let busy = app.running || matches!(app.status, crate::app::AgentStatus::Running { .. });
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
            degrade(frame_logs, name);
            *tick = None;
        }
        for note in out.notes {
            app.push_system(&note);
            *dirty = true;
        }
        // The face is assigned unconditionally: `None` clears it, so a
        // panel can retract its status line without a sentinel.
        if app.panel_status != out.status {
            app.panel_status = out.status;
            *dirty = true;
        }
        if let Some(syn) = out.synthetic
            && !app.running
        {
            tracing::info!("tui: panel synthetic -> user answer");
            // Same commit rule as the Submit send site: user-turn state
            // lands here, and a click answer counts toward the title
            // threshold (`Synthetic::user`).
            app.begin_user_turn(&syn.text);
            // Provenance rides `Synthetic::user`: a click answer is
            // real user intent (trail-worthy); a generated report is
            // machine-composed (trail-synthetic).
            if cmd_tx
                .send(Cmd::Run {
                    input: syn.text,
                    synthetic: !syn.user,
                    images: Vec::new(),
                })
                .is_ok()
            {
                app.running = true;
                if syn.user {
                    *user_msg_count += 1;
                }
            } else {
                app.settle_status_from_inflight();
            }
            *dirty = true;
        }
        *dirty |= out.dirty;
    }

    // Death-sweep backstop (Rev 2026-11-04): rows go at the design's
    // death points; the tick covers any path that changed row state
    // without passing an event handler.
    if app.auto_sweep() {
        *dirty = true;
    }

    // Reconcile background shell tasks from the registry (tick polling).
    if app.reconcile_background_tasks() {
        *dirty = true;
    }

    // Reconcile the steering mirror: what the engine drained at the
    // last tool boundary is now the model's input, so it leaves the
    // pending strip and lands in the transcript (one echo per message,
    // at delivery — not at queue time).
    if app.sync_steer_queue(agent.runtime().steering_snapshot()) {
        *dirty = true;
    }

    // Detect a dead agent task. `agent_task` is only awaited at exit,
    // so a panic that escaped tool-level isolation would otherwise
    // leave the spinner turning forever with the user typing into a
    // dead session. `outcome.is_none()` skips the normal quit path,
    // where Quit is sent to the agent right before the UI exits.
    if outcome.is_none() && !*agent_dead_announced && agent_task.is_finished() {
        *agent_dead_announced = true;
        tracing::error!("tui: agent task finished unexpectedly (panic or early exit)");
        app.push_system(
            "! Agent runtime terminated unexpectedly - input is no longer \
                     processed. Quit (Ctrl+C) and resume the session; see session.log \
                     for the panic details.",
        );
        *dirty = true;
    }

    // Drain queued approval requests into the popup queue (one popup at a
    // time — the front of the queue is what gets rendered). The product
    // panel's consent prompts ride their own channel (SessionBoot)
    // into the same queue: they are prompts like any other, only their
    // key legend differs.
    let had_approval = app.has_pending_approval();
    while let Ok(item) = approval_rx.try_recv() {
        app.approval_queue.push_back(item);
        *dirty = true;
    }
    if let Some(consent_rx) = consent_rx.as_mut() {
        while let Ok(item) = consent_rx.try_recv() {
            app.approval_queue.push_back(item);
            *dirty = true;
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
            *dirty = true;
        }
    }
    // Product pick modals (the `/model` chooser): pushed by a command action
    // parked on its oneshot, exactly like an ask parked on its answer.
    let had_pick = app.has_pending_pick();
    while let Ok(item) = pick_rx.try_recv() {
        app.push_pick(item);
        *dirty = true;
    }
    // BUG 4: a popup that just opened must never see keys typed before it
    // appeared — those were aimed at the composer, not at a prompt nobody
    // had seen yet (`/plugin`'s trailing n once decided Deny this way).
    // Drop whatever is still buffered and keep Resizes (they self-heal in
    // the 1 Hz geometry probe anyway if they were lost). Keys that survive
    // into the popup can at worst select an option — bare Enter is still
    // required to commit.
    let popup_opened = (!had_approval && app.has_pending_approval())
        || (!had_ask && app.has_pending_ask())
        || (!had_pick && app.has_pending_pick());
    if popup_opened {
        let mut dropped_keys = 0usize;
        while crossterm::event::poll(Duration::ZERO)? {
            match crossterm::event::read()? {
                Event::Resize(w, h) => {
                    tracing::info!(w, h, "tui: resize during popup-open input drain");
                    terminal.resize(Rect::new(0, 0, w, h))?;
                    terminal.clear()?;
                    *last_size = (w, h);
                    *dirty = true;
                }
                Event::Key(_) | Event::Paste(_) => dropped_keys += 1,
                _ => {}
            }
        }
        if dropped_keys > 0 {
            tracing::info!(dropped_keys, "tui: drained pre-popup input at popup open");
        }
    }

    let (crossterm_count, event_types) = drain_input(
        app,
        terminal,
        frame_logs,
        agent,
        event_tx,
        cmd_tx,
        commands,
        ask_rx,
        consent_rx,
        approval_rx,
        pick_rx,
        update,
        session_io,
        on_submit,
        on_reopen,
        title_gen,
        session,
        session_id,
        session_dir,
        base_dir,
        clipboard,
        dirty,
        last_size,
        saw_scroll,
        user_msg_count,
        outcome,
    )
    .await?;
    // If no events arrived, sleep briefly to avoid busy-spinning.
    let slept = !*dirty;
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
        *last_geom_check = Instant::now();
        let probed = crossterm::terminal::size().unwrap_or(*last_size);
        if probed != *last_size {
            tracing::info!(?last_size, ?probed, "tui: geometry self-heal resize");
            *last_size = probed;
            terminal.resize(Rect::new(0, 0, probed.0, probed.1))?;
            terminal.clear()?;
            *dirty = true;
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
        *last_term_guard = Instant::now();
        if !term_guard::raw_mode_intact() {
            tracing::warn!("tui: termios drifted out of raw mode; re-asserting");
            term_guard::reassert_raw_mode();
            terminal.clear()?;
            *dirty = true;
        }
    }

    // Opportunistic tips refresh (see TIPS_RECHECK): a session that
    // outlives the refresh interval gets another background pass;
    // the cache timestamp keeps a fresh one a no-op.
    if let Some(tips) = tips
        && last_tips_check.elapsed() >= TIPS_RECHECK
    {
        *last_tips_check = Instant::now();
        spawn_tips_refresh(&tips.fetch, event_tx.clone());
    }

    // Liveness tick: active (Running/Waiting) with no events this
    // iteration → redraw so spinner/elapsed advance. Tick-only frames are
    // drawn but not captured (see UI_TICK).
    let mut tick_fired = false;
    if !*dirty && app.is_active() && last_draw.elapsed() >= UI_TICK {
        *dirty = true;
        tick_fired = true;
    }

    // Only redraw when state actually changed (dirty flag). With a 2 ms
    // poll timeout the idle loop would otherwise burn ~500 useless draws/s.
    let mut draw_elapsed = Duration::ZERO;
    if *dirty {
        *last_draw = Instant::now();
        let draw_start = Instant::now();
        // T12: render-path product hooks (CopyTable formatters, the
        // consent classifier) panic out of product code — skip the
        // frame and trace it, never unwind through the loop. An io
        // error from the terminal itself still propagates (`?`).
        let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // `.map(|_| ())` so the frame's borrow of `terminal` does
            // not escape the closure.
            terminal.draw(|f| render::draw(f, app)).map(|_| ())
        }));
        match drawn {
            Ok(result) => {
                result?;
            }
            Err(_) => degrade(frame_logs, "terminal.draw"),
        }
        draw_elapsed = draw_start.elapsed();

        // Log composer state after each draw (only when state changes).
        if let Some(logs) = &mut *frame_logs {
            let size = terminal.size()?;
            logs.composer
                .record(app, logs.frames.frame_id(), (size.width, size.height));
        }
    }

    // Record the frame if it changed since the last one (dedup keeps the
    // flipbook small — steady states and per-token deltas collapse away).
    // Gated on `dirty` + the capture interval so the offscreen render only
    // runs after a real change, at most ~10×/s.
    let mut capture_elapsed = Duration::ZERO;
    if *dirty && let Some(logs) = &mut *frame_logs {
        let size = terminal.size()?;
        // T12: the offscreen snapshot walks the same render path —
        // a panicking product formatter skips this capture, traced.
        let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            logs.frames
                .capture(!tick_fired, app, size.width, size.height)
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
    if let Some(logs) = &mut *frame_logs {
        logs.perf.record(&PerfRow {
            frame_id: logs.frames.frame_id(),
            draw_ms: draw_elapsed.as_millis(),
            capture_ms: capture_elapsed.as_millis(),
            loop_ms: loop_elapsed.as_millis(),
            dirty: *dirty,
            scroll_offset: app.viewport.scroll_offset,
            follow_bottom: app.viewport.follow_bottom,
            output_lines: app.transcript.len(),
            crossterm_events: crossterm_count,
            slept,
            event_types: &event_types,
        });
    }
    // Reset dirty after draw + optional capture so idle loops skip both.
    *dirty = false;
    Ok(())
}
