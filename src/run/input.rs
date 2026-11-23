//! Crossterm input polling folded into app state (one bounded drain).

use super::*;
use crate::app::PasteOutcome;

#[allow(clippy::too_many_arguments)]
pub(super) async fn drain_input(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    frame_logs: &mut Option<FrameLogs>,
    agent: &Arc<PhiAgent>,
    event_tx: &mpsc::UnboundedSender<TuiEvent>,
    cmd_tx: &mpsc::UnboundedSender<Cmd>,
    commands: &[CommandSpec],
    ask_rx: &mut Option<mpsc::UnboundedReceiver<AskItem>>,
    consent_rx: &mut Option<mpsc::UnboundedReceiver<ApprovalItem>>,
    approval_rx: &mut mpsc::UnboundedReceiver<ApprovalItem>,
    pick_rx: &mut mpsc::UnboundedReceiver<PickItem>,
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
    last_size: &mut (u16, u16),
    saw_scroll: &mut bool,
    user_msg_count: &mut usize,
    outcome: &mut Option<TuiOutcome>,
) -> Result<(u32, String)> {
    // Poll input with a short timeout so the frame rate stays bounded even
    // while a long turn streams events. 2 ms keeps scroll responsive (~500 Hz
    // poll rate, gated by draw cost); the old 16 ms added perceptible jank
    // on mouse-wheel scrolling because each event blocked for a full frame.
    //
    // Drain ALL pending crossterm events in one go — `read()` returns one
    // event at a time, so a fast scroll-wheel burst leaves the rest queued.
    // Without this loop, each leftover event waits an extra 2 ms poll cycle,
    // creating visible stutter on track-pad / inertial scrolling.
    //
    // Collect the batch before dispatching any of it: the scroll-companion
    // guard must see the whole batch first (a synthesized ↑/↓ may lead its
    // wheel event), and the `/resume` picker break below then drops the
    // un-dispatched tail with the same "keys can at worst select an option"
    // posture as the popup-open drain in pump.rs.
    let mut batch = Vec::new();
    while crossterm::event::poll(Duration::ZERO)? {
        batch.push(crossterm::event::read()?);
    }
    let crossterm_count = batch.len() as u32;
    let mut has_scroll = false;
    let mut has_key = false;
    let mut has_mouse = false;
    let mut has_paste = false;
    let mut has_resize = false;
    // Pre-scan the batch for wheel activity BEFORE the first key dispatch:
    // this is what makes companion-arrow suppression order-independent.
    app.note_input_batch(&batch);
    for event in batch {
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                *dirty = true;
                has_key = true;
                if let Some(action) = app.handle_key_event(key.code, key.modifiers, key.kind) {
                    match action {
                        Action::Submit(text) => {
                            // Intercept built-in commands before sending to agent.
                            // `/resume` exists only for session-based products
                            // (O6: 不填即关) — no slot means no command.
                            if text.trim() == "/resume"
                                && let Some(session_io) = session_io
                            {
                                let picked = crate::picker::show_picker_from_tui(
                                    base_dir,
                                    Some(session_id),
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
                                    *outcome = Some(TuiOutcome::SwitchSession(picked_dir));
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
                                && let Some(update) = update
                            {
                                let check = Arc::clone(&update.check);
                                let update_tx = event_tx.clone();
                                // T12: same degrade as the auto check —
                                // guarded call phase, supervised future.
                                let (fut, degraded) = hook_guard(
                                    "update.check",
                                    || check(),
                                    Box::pin(std::future::ready(None))
                                        as crate::shell::TaskFuture<Option<String>>,
                                );
                                if let Some(hook) = degraded {
                                    let _ = update_tx.send(TuiEvent::Degrade(hook));
                                } else {
                                    let hint_tx = update_tx.clone();
                                    spawn_traced(
                                        "update.check",
                                        Box::pin(async move {
                                            if let Some(hint) = fut.await {
                                                let _ = hint_tx.send(TuiEvent::StatusHint(hint));
                                            }
                                        }),
                                        update_tx,
                                    );
                                }
                                // Not a `break`: the rest of the batch still
                                // belongs to the composer (a queued keystroke
                                // must not vanish because an /upgrade landed
                                // next to it in one drain). `continue` skips
                                // the agent-send below for this event only.
                                continue;
                            }
                            // Enter-boundary product rewrite (design
                            // §3): pending panel picks merge behind
                            // the typed text as one user message
                            // (typed first, blank line, then the pick
                            // lines); `None` = submit unchanged. The
                            // product also settles its waiting summary
                            // inside the hook.
                            let text = match on_submit {
                                Some(hook) => {
                                    // T12: panic degrades to "submit
                                    // unchanged", traced to the log.
                                    let (rewritten, degraded) =
                                        hook_guard("on_submit", || hook(&text), None);
                                    if let Some(name) = degraded {
                                        degrade(frame_logs, name);
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
                            // Pasted screenshots ride this message: the chips
                            // clear with the send (a rescue puts them back —
                            // the rescue arm holds them).
                            let images = app.take_pending_images();
                            app.begin_rescuable_turn_with_images(&text, &images);
                            // History (#7): record what actually goes out —
                            // an intercepted submit (/resume, /upgrade) never
                            // reaches this line and leaves no history.
                            app.record_input(&text);
                            if cmd_tx
                                .send(Cmd::Run {
                                    input: text,
                                    synthetic: false,
                                    images,
                                })
                                .is_ok()
                            {
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
                            *user_msg_count += 1;
                            if *user_msg_count == 10 {
                                // Fire-and-forget: the shell owns the
                                // milestone trigger (the 10th user
                                // turn); what "generate a title" means
                                // — model, wording, persistence — is
                                // product strategy behind the
                                // TitleGenerator fn field (T5d).
                                if let Some(title) = title_gen {
                                    let msgs = agent
                                        .runtime()
                                        .get_messages(session)
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
                                        || (title.generate)(msgs, session_dir.to_path_buf()),
                                        Box::pin(std::future::ready(()))
                                            as crate::shell::TaskFuture,
                                    );
                                    if let Some(hook) = degraded {
                                        degrade(frame_logs, hook);
                                    } else {
                                        // Supervised spawn: a panic
                                        // inside the generator's future
                                        // is traced, not lost with a
                                        // detached task.
                                        spawn_traced("title.generate", fut, event_tx.clone());
                                    }
                                }
                            }
                        }
                        Action::Approve(decision) => app.approve_front(decision),
                        Action::CopyLastReply => {
                            let text = app.last_reply_text();
                            copy_text(app, clipboard, text);
                        }
                        Action::CopySelection => {
                            let text = app.selection_text();
                            copy_text(app, clipboard, text);
                            app.clear_selection();
                            tracing::info!("selection cleared after copy");
                        }
                        Action::PasteImage => {
                            *dirty = true;
                            let grabbed = crate::clipboard_image::grab_clipboard_image();
                            if app.apply_pasted_image(grabbed, session_dir) == PasteOutcome::NoImage
                            {
                                // No image on the clipboard: fall back to a
                                // plain text paste, the same insert the
                                // bracketed-paste event does.
                                if let Some(cb) = clipboard.as_mut()
                                    && let Ok(text) = cb.get_text()
                                    && !text.is_empty()
                                {
                                    app.paste(&text);
                                }
                            }
                        }
                        Action::Redraw => {
                            // Same repair as the Resize path: clear
                            // invalidates ratatui's diff cache so every
                            // cell goes back out, overwriting whatever
                            // the terminal clipped or left stale.
                            tracing::info!("ctrl+l: forced full repaint");
                            terminal.clear()?;
                            *dirty = true;
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
                            if let Some(reopen) = on_reopen {
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
                                    degrade(frame_logs, name);
                                }
                                *dirty = true;
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
                            // A pick nobody is waiting on any more: the
                            // parked command action is answered `Cancelled`.
                            app.clear_picks();
                            while pick_rx.try_recv().is_ok() {}
                            // Cancel-rescue (#6): a turn cancelled before it
                            // produced anything is a typing mistake, not a
                            // conversation — the message comes back to the
                            // composer and the echo is rolled back, as if
                            // never sent (CC auto-restore semantics: only
                            // when no text/tool call was produced).
                            if let Some(text) = app.take_cancel_rescue() {
                                app.take_back_into_composer(vec![text]);
                                *dirty = true;
                            }
                        }
                        Action::Steer(text) => {
                            app.record_input(&text);
                            if text.trim_start().starts_with('/') {
                                // A raw `/foo` steering message is just
                                // text to the model — never steer it. The
                                // mid-turn lane runs interactive commands
                                // NOW (their pick pops at once); the rest
                                // hold and run as their own turn at the end.
                                // A repeat of a command already waiting is
                                // an impatient key, not a second intent:
                                // hold once, say so.
                                if try_mid_turn_command(app, &text, commands, event_tx) {
                                    // Ran now (or refused as a running dup).
                                } else if app.hold_command(text) {
                                    app.set_notice("Command queued - runs after the current turn");
                                } else {
                                    app.set_notice(
                                        "Command already queued - runs after the current turn",
                                    );
                                }
                            } else {
                                agent.runtime().steer(text.clone());
                                app.queued_steer.push(text);
                                app.set_notice(
                                    "Queued - delivered at the next step (Up takes it back)",
                                );
                            }
                            *dirty = true;
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
                            *dirty = true;
                        }
                        Action::ForceSend(text) => {
                            app.record_input(&text);
                            if !text.trim().is_empty() {
                                if text.trim_start().starts_with('/') {
                                    // Classic lane even for mid-turn-flagged
                                    // commands: this key also cancels the turn
                                    // and clears every modal below (a just
                                    // spawned pick would be eaten there). The
                                    // turn dies in milliseconds and the held
                                    // command rides the flush right after.
                                    app.hold_command(text);
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
                            app.clear_picks();
                            while pick_rx.try_recv().is_ok() {}
                            *dirty = true;
                        }
                        Action::Quit => {
                            agent.cancel();
                            let _ = cmd_tx.send(Cmd::Quit);
                            *outcome = Some(TuiOutcome::Quit);
                        }
                    }
                }
            }
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                ..
            }) => {
                has_scroll = true;
                *saw_scroll = true;
                if app.scroll_wheel_up() {
                    *dirty = true;
                }
            }
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                ..
            }) => {
                has_scroll = true;
                *saw_scroll = true;
                if app.scroll_wheel_down() {
                    *dirty = true;
                }
            }
            Event::Mouse(MouseEvent {
                kind, column, row, ..
            }) => {
                *dirty = true;
                has_mouse = true;
                // The copy-menu hit-test needs the terminal size to locate
                // the popup exactly where it was drawn.
                let size = terminal.size()?;
                if let Some(action) = app.handle_mouse(kind, column, row, size.width, size.height)
                    && let Action::CopySelection = action
                {
                    let text = app.selection_text();
                    copy_text(app, clipboard, text);
                    app.clear_selection();
                }
            }
            Event::Paste(text) => {
                *dirty = true;
                has_paste = true;
                // A paste event with no text is a terminal keeping the
                // paste key for itself over a non-text clipboard (macOS
                // Cmd+V pastes a picture as nothing): try the clipboard's
                // image before giving up. Non-empty pastes are text,
                // always — copied words never turn into a picture.
                if text.is_empty() {
                    let grabbed = crate::clipboard_image::grab_clipboard_image();
                    let _ = app.apply_pasted_image(grabbed, session_dir);
                } else {
                    app.paste(&text);
                }
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
                *last_size = (w, h);
                *dirty = true;
            }
            _ => {}
        }
    }
    // Edge auto-scroll: a left-drag pinned to the output pane's top/bottom
    // edge keeps scrolling one row per tick even when no further mouse
    // events arrive (the whole point — the pointer stops, the content does
    // not). Self-paced inside; runs every drain (~2 ms).
    if app.edge_autoscroll_step(Instant::now()) {
        *dirty = true;
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

    Ok((crossterm_count, event_types))
}

/// The mid-turn command lane: a `mid_turn` command typed while a turn is
/// streaming executes NOW — its pick pops at once instead of waiting for the
/// turn to settle. Returns `true` when the input was taken: spawned, or
/// refused as the duplicate of the same command already executing there.
///
/// The action runs detached from the turn loop and answers through
/// [`TuiEvent::SystemLine`] — never [`TuiEvent::SystemMessage`], which would
/// falsely settle the streaming turn. [`ShellFlow::Unhandled`] cannot fall
/// through to a model turn mid-turn (that path would commit turn state from
/// a send site — the phantom-state bug class), so a decline degrades to a
/// system line. The claim slot is released once the action answers.
fn try_mid_turn_command(
    app: &mut App,
    text: &str,
    commands: &[CommandSpec],
    event_tx: &mpsc::UnboundedSender<TuiEvent>,
) -> bool {
    let Some(spec) = commands.iter().find(|c| c.mid_turn && c.matches(text)) else {
        return false;
    };
    if !app.claim_mid_command(text) {
        app.set_notice("Command already running - one at a time");
        return true;
    }
    // The command runs now, so it echoes now (the classic lane echoes at
    // its flush send site instead).
    app.push_user(text);
    let action = Arc::clone(&spec.action);
    let name = spec.name.clone();
    let input = text.to_string();
    let claims = Arc::clone(&app.mid_commands);
    let event_tx = event_tx.clone();
    tokio::spawn(async move {
        let input_for_action = input.clone();
        // T12: a panicking command action degrades to `ShellFlow::Unhandled`
        // — traced, and here that lands on the decline line below.
        let (flow, degraded) = hook_guard_async(
            "spec.action",
            move || action(input_for_action),
            ShellFlow::Unhandled,
        )
        .await;
        if let Some(hook) = degraded {
            let _ = event_tx.send(TuiEvent::Degrade(hook));
        }
        let line = match flow {
            ShellFlow::Answer(answer) => answer,
            ShellFlow::Unhandled => format!("/{name} declined the input"),
        };
        let _ = event_tx.send(TuiEvent::SystemLine(line));
        claims.lock().unwrap().remove(&input);
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::CommandAction;

    fn spec(name: &str, mid_turn: bool, action: CommandAction) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            help: String::new(),
            in_picker: false,
            action,
            mid_turn,
        }
    }

    fn answer_action(answer: &str) -> CommandAction {
        let answer = answer.to_string();
        Arc::new(move |_| Box::pin(std::future::ready(ShellFlow::Answer(answer.clone()))))
    }

    /// protects: the mid-turn lane runs a flagged command while the turn
    /// streams — it echoes now and answers through a non-settling system
    /// line, holding its claim until the answer lands. fails_when: the
    /// command waits for the turn, or its answer settles the turn.
    /// no_claim: the action's own copy.
    #[tokio::test]
    async fn the_mid_turn_lane_runs_a_flagged_command_now() {
        let mut app = App::new();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let commands = vec![spec("model", true, answer_action("pick popped"))];
        assert!(try_mid_turn_command(
            &mut app, "/model", &commands, &event_tx
        ));
        assert!(
            app.transcript
                .output
                .iter()
                .any(|l| l.kind == phi_tui::lines::LineKind::User && l.text.contains("/model")),
            "the command echoes now, not at some later send site"
        );
        assert!(
            app.mid_commands.lock().unwrap().contains("/model"),
            "the claim is held while the action runs"
        );
        let ev = event_rx.recv().await.expect("the action answers");
        assert!(
            matches!(&ev, TuiEvent::SystemLine(s) if s == "pick popped"),
            "answered via SystemLine, got {ev:?}"
        );
        assert!(
            !app.mid_commands.lock().unwrap().contains("/model"),
            "the claim is released when the action answers"
        );
    }

    /// protects: a command without the flag stays in the classic lane — the
    /// caller holds it for the post-turn flush instead. fails_when: every
    /// command spawns mid-turn. no_claim: the queue's own behavior.
    #[tokio::test]
    async fn an_unflagged_command_stays_in_the_classic_lane() {
        let mut app = App::new();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let commands = vec![spec("plugin", false, answer_action("late"))];
        assert!(!try_mid_turn_command(
            &mut app, "/plugin", &commands, &event_tx
        ));
        assert!(event_rx.try_recv().is_err(), "nothing was spawned");
    }

    /// protects: the same command already executing in the lane is refused,
    /// not duplicated — a hammered Enter is one pick, not two. fails_when:
    /// the repeat spawns a second action. no_claim: the notice wording.
    #[tokio::test]
    async fn a_running_mid_command_is_refused_not_duplicated() {
        let mut app = App::new();
        assert!(app.claim_mid_command("/model"));
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let commands = vec![spec("model", true, answer_action("second pick"))];
        assert!(try_mid_turn_command(
            &mut app, "/model", &commands, &event_tx
        ));
        assert!(event_rx.try_recv().is_err(), "the duplicate spawns nothing");
    }

    /// protects: an action that declines the input cannot fall through to a
    /// model turn mid-turn — it degrades to a system line naming the
    /// command. fails_when: a decline starts a second turn while one
    /// streams. no_claim: the decline wording.
    #[tokio::test]
    async fn a_declined_mid_command_answers_with_a_decline_line() {
        let mut app = App::new();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let commands = vec![spec(
            "model",
            true,
            Arc::new(|_| Box::pin(std::future::ready(ShellFlow::Unhandled))),
        )];
        assert!(try_mid_turn_command(
            &mut app, "/model", &commands, &event_tx
        ));
        let ev = event_rx.recv().await.expect("the decline still answers");
        assert!(
            matches!(&ev, TuiEvent::SystemLine(s) if s == "/model declined the input"),
            "got {ev:?}"
        );
    }
}
