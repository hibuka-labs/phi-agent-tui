//! The agent-side turn runner: one background task for the TUI's lifetime.

use super::*;

/// The agent-side task: run turns on command, forward events to the TUI, and
/// persist per-turn JSONL logs exactly like the REPL path does.
// Turn-side wire-up: a working subset of `run()`'s unpacked handles; the allow covers this long list.
#[allow(clippy::too_many_arguments)]
pub(super) async fn agent_loop(
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
            Cmd::Run {
                input,
                synthetic: synthetic_run,
                images: pending_images,
            } => {
                // Product command face (T5d): a `/name` submit is offered to
                // the product's command table first — local semantics,
                // settled via SystemMessage (no turn_number), never a model
                // turn. Commands serialize on this loop, so the socket board
                // sees one at a time; a plugin install blocks here until the
                // human answers its full-facts confirm popup. Synthetic input
                // (machine-composed) is never offered: a child report that
                // happens to start with "/" must not become a command.
                if !synthetic_run && let Some(spec) = commands.iter().find(|c| c.matches(&input)) {
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
                    // Synthetic input is always a plain turn: never skill
                    // routing either (same reasoning as the command face).
                    _ if synthetic_run => RouteOutcome::Turn { input },
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

                // Pasted screenshots ride the user message through the vision
                // channel: read the captures into attachments now. Synthetic
                // turns never carry images, and a capture that cannot be read
                // is a visible warning, never a silent drop.
                let (attachments, attach_warnings, image_labels) = if synthetic_run {
                    (Vec::new(), Vec::new(), String::new())
                } else {
                    let (attachments, warnings) = attachments_from(&pending_images);
                    (attachments, warnings, image_label_lines(&pending_images))
                };
                for warning in attach_warnings {
                    let _ = event_tx.send(TuiEvent::Warning(warning));
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
                        // An ephemeral message leaves no trace at turn end, so
                        // there is nothing to hang pictures on: refuse them
                        // visibly instead of sending them into the void.
                        if !attachments.is_empty() {
                            let _ = event_tx.send(TuiEvent::Warning(format!(
                                "{} image(s) were not sent - a turn-scope skill run leaves no message to attach to",
                                attachments.len()
                            )));
                        }
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
                        // The picture labels ride the body into history: the
                        // durable capture path follows the pixels, so an
                        // evicted picture stays recoverable.
                        let body = if image_labels.is_empty() {
                            body
                        } else {
                            format!("{image_labels}\n{body}")
                        };
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
                            .run_turn_with_images(session.clone(), &body, attachments, move |ev| {
                                turn_events_clone.lock().unwrap().push(ev);
                                Ok(())
                            })
                            .await
                    }
                    RouteOutcome::Turn { input: body } => {
                        // Synthetic runs land here (routing is bypassed above)
                        // and keep their tag through the engine entry.
                        if synthetic_run {
                            agent
                                .run_turn_synthetic_input(session.clone(), &body, move |ev| {
                                    turn_events_clone.lock().unwrap().push(ev);
                                    Ok(())
                                })
                                .await
                        } else {
                            // Same label ride as the baked arm — only real
                            // turns carry pictures (synthetic runs never do).
                            let body = if image_labels.is_empty() {
                                body
                            } else {
                                format!("{image_labels}\n{body}")
                            };
                            agent
                                .run_turn_with_images(
                                    session.clone(),
                                    &body,
                                    attachments,
                                    move |ev| {
                                        turn_events_clone.lock().unwrap().push(ev);
                                        Ok(())
                                    },
                                )
                                .await
                        }
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

                let _ = event_tx.send(turn_end_event(result));
            }
        }
    }
}

/// Map a turn result onto the settle event the UI sees. A user cancel
/// (`AgentError::Cancelled`) is not an error and never a plain done: it
/// settles as `TurnCancelled`, which stamps the `cancelled` verdict on the
/// turn-final line regardless of delivery order — the engine's `RunCancelled`
/// (which paints the marker row, or stays silent when the cancel was
/// rescued) travels a separate bus hop and can arrive late.
pub(super) fn turn_end_event(result: AgentResult<RunOutcome>) -> TuiEvent {
    match result {
        Ok(RunOutcome::Failed { error }) => TuiEvent::TurnError(error),
        Ok(RunOutcome::MaxTurnsExceeded { turns }) => {
            TuiEvent::TurnError(format!("max turns ({turns}) exceeded"))
        }
        Ok(RunOutcome::Cancelled) => TuiEvent::TurnCancelled,
        Err(e) if e.is_cancelled() => TuiEvent::TurnCancelled,
        Ok(_) => TuiEvent::TurnDone,
        Err(e) => TuiEvent::TurnError(format!("{e}")),
    }
}

/// Read pasted captures into vision-channel attachments. A capture that is
/// gone or is not a PNG/JPEG image is reported in the warnings (visible —
/// never a silent drop); the rest ride the user message.
fn attachments_from(
    pending: &[crate::app::PendingImage],
) -> (Vec<phi_agent::ImageAttachment>, Vec<String>) {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for img in pending {
        match std::fs::read(&img.path) {
            Ok(bytes) => match phi_agent::image_ops::sniff_mime(&bytes) {
                Some(mime) => out.push(phi_agent::ImageAttachment::Base64 {
                    data: phi_agent::image_ops::to_base64(&bytes),
                    media_type: Some(mime.to_string()),
                    detail: None,
                }),
                None => warnings.push(format!(
                    "image {} was not sent - not a PNG/JPEG image",
                    img.path.display()
                )),
            },
            Err(e) => warnings.push(format!("image {} was not sent - {e}", img.path.display())),
        }
    }
    (out, warnings)
}

/// One label line per attached picture: the durable capture path follows the
/// image into history, so an evicted picture stays recoverable. The format is
/// a machine contract — the window-reset residue parser
/// (`agent_works::rotation_policy::image_residue`) reads the `path="..."`
/// fields back out.
pub fn image_label_lines(pending: &[crate::app::PendingImage]) -> String {
    pending
        .iter()
        .enumerate()
        .map(|(i, img)| format!("<image name={} path=\"{}\">", i + 1, img.path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}
