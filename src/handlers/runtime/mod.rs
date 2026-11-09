//! Runtime event handler for the TUI application.
//!
//! Handles `RuntimeEvent` from the agent, updating the application state
//! accordingly. This includes:
//! - Text streaming (TextDelta, ThoughtDelta)
//! - Tool calls (ActionStarted, ActionFinished)
//! - Plan updates (PlanUpdated)
//! - Approval requests (AwaitingApproval)
//! - Run lifecycle (RunFinished, RunCancelled)
//! - User events (Progress, Notice)

mod tool_text;

pub(crate) use tool_text::agent_prefix;
use tool_text::{
    bg_start_result_label, build_tool_detail, notice_line, settle_tool_call, spawn_result_label,
    tool_invocation_text,
};

use std::time::Instant;

use phi_agent::RuntimeEvent;
use phi_agent::{NoticeKind, PlanStepStatus, UserEvent};
use phi_tui::lines::{LineDetail, LineKind, MetaHead, OutputLine, ToolState};
use phi_tui::wrap::{one_line, wrap};

use crate::app::{AgentStatus, App, Phase, SubAgentStatus, ToolEvent};
use crate::theme::ShellToken;

impl App {
    /// Reflect streaming activity in the status strip — root events only.
    /// Child events arrive through the persistent bus subscription even while
    /// the root is Waiting/Idle; they must not flip the root's state back to
    /// Running.
    fn note_activity(&mut self, agent_id: Option<&str>, phase: Phase) {
        if agent_id.is_none_or(|id| id.is_empty()) {
            // Start the stretch's clock on the first busy signal — not only on
            // Idle → Running: the user-send path and an approval grant set the
            // status directly, and a stretch that never started its clock
            // renders a frozen spinner with no elapsed suffix (the perceived
            // hang of session 20261006_9264ba3e).
            self.begin_activity();
            self.status = AgentStatus::Running { phase };
        }
    }

    /// Handle a runtime event from the agent.
    pub fn handle_runtime(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::TextDelta { text, agent_id, .. } => {
                self.track_agent(agent_id.as_deref());
                let child = agent_id.as_deref().filter(|id| !id.is_empty());
                match child {
                    // Child deltas accumulate per-child (never in the shared
                    // `stream`: its pending tail IS the main view's live tail,
                    // and a minutes-long child run flooded it — session
                    // 20260904_3eeb5610). Flushed lines land in the child's
                    // own transcript; visibility = task panel, not the main view.
                    Some(id) => {
                        let had_thought = self
                            .child_streams
                            .get(id)
                            .is_some_and(|s| s.has_pending_thought());
                        let flushed = self.push_child_text(id, &text);
                        self.sub_agent_transcripts
                            .entry(id.to_string())
                            .or_default()
                            .extend(flushed);
                        // push_text implicitly flushes a pending thought first
                        // (StreamState-internal, invisible to flush_pending) —
                        // retire this stream's panel timer so the elapsed
                        // readout doesn't outlive its segment.
                        if had_thought {
                            self.thinking_since.remove(id);
                        }
                    }
                    None => {
                        let had_thought = self.stream.has_pending_thought();
                        let flushed = self.stream.push_text(&text, agent_id.as_deref());
                        for line in flushed {
                            self.transcript.push(line);
                        }
                        // push_text implicitly flushes a pending thought first
                        // (StreamState-internal, invisible to flush_pending) —
                        // retire this stream's panel timer so the elapsed
                        // readout doesn't outlive its segment.
                        if had_thought {
                            self.thinking_since.remove("");
                        }
                    }
                }
                self.note_activity(agent_id.as_deref(), Phase::Streaming);
            }
            RuntimeEvent::ThoughtDelta { text, agent_id, .. } => {
                self.track_agent(agent_id.as_deref());
                let child = agent_id.as_deref().filter(|id| !id.is_empty());
                match child {
                    Some(id) => {
                        let was = self
                            .child_streams
                            .get(id)
                            .is_some_and(|s| s.has_pending_thought());
                        let flushed = self.push_child_thought(id, &text);
                        // push_thought implicitly flushes the previous segment on a
                        // kind/agent flip — that starts a NEW segment, so re-arm the
                        // timer even when `was`, or elapsed overstates the new
                        // segment's age.
                        let flushed_thought = flushed
                            .iter()
                            .any(|l| l.kind == phi_tui::lines::LineKind::Thought);
                        self.sub_agent_transcripts
                            .entry(id.to_string())
                            .or_default()
                            .extend(flushed);
                        if !was || flushed_thought {
                            self.thinking_since.insert(id.to_string(), Instant::now());
                        }
                    }
                    None => {
                        let was = self.stream.has_pending_thought();
                        let flushed = self.stream.push_thought(&text, agent_id.as_deref());
                        // Same implicit-flush re-arm as the child arm (e.g. a root
                        // thought arriving under a different `agent_id` tag).
                        let flushed_thought = flushed
                            .iter()
                            .any(|l| l.kind == phi_tui::lines::LineKind::Thought);
                        for line in flushed {
                            self.transcript.push(line);
                        }
                        if !was || flushed_thought {
                            self.thinking_since.insert(String::new(), Instant::now());
                        }
                    }
                }
                self.note_activity(agent_id.as_deref(), Phase::Thinking);
            }
            RuntimeEvent::ActionStarted {
                tool_name,
                args_json,
                agent_id,
                ..
            } => {
                self.flush_pending();
                // Child tool call: commit that child's pending stream first so
                // its text lands above the invocation line in its transcript.
                if let Some(id) = agent_id.as_deref().filter(|id| !id.is_empty()) {
                    let flushed = self.flush_child_stream(id);
                    self.sub_agent_transcripts
                        .entry(id.to_string())
                        .or_default()
                        .extend(flushed);
                }
                // Fresh tool call → drop any progress from the previous one.
                self.live_progress = None;
                // Root calls materialize together, after the stream drains —
                // the drafting window is over. Child calls must not clear the
                // root's drafts.
                if agent_id.as_deref().filter(|id| !id.is_empty()).is_none() {
                    self.tool_drafts.clear();
                }
                // Tools render inline in the transcript (Claude Code style): an
                // invocation line now, a result line on `ActionFinished`.
                self.track_agent(agent_id.as_deref());
                let prefix = agent_prefix(agent_id.as_deref());
                // Update SubAgentState events
                if let Some(id) = agent_id.as_deref()
                    && let Some(state) = self.sub_agents.get_mut(id)
                {
                    state.last_tool_at = Instant::now();
                    state.events.push(ToolEvent {
                        tool_name: tool_name.clone(),
                        summary: String::new(),
                        is_finished: false,
                    });
                }
                // `update_plan` renders as a plan block (see `PlanUpdated`), so both
                // its invocation line (raw JSON args) and result line are noise —
                // suppress them. Its status transition is still applied.
                if tool_name != "update_plan" {
                    // For payload tools, attach the structured block (edit diff /
                    // written content / spawn brief) the ladder expands below
                    // this line.
                    let detail = build_tool_detail(&tool_name, &args_json);
                    let body = tool_invocation_text(
                        &tool_name,
                        &args_json,
                        &prefix,
                        self.transcript.wrap_width(),
                        &self.workspace_root,
                    );
                    // The `*` is a marker slot, not a glyph: the renderer draws
                    // the state glyph over it (braille while running, `o`/`x`
                    // when settled). Keeping a stable placeholder means the
                    // line's plain text is self-describing in the log.
                    let text = format!("* {body}");
                    let line = OutputLine {
                        spans: None,
                        original: None,
                        detail,
                        text,
                        kind: LineKind::Tool,
                        tool_state: Some(ToolState::Running),
                    };
                    // Route to the correct transcript
                    if let Some(id) = agent_id.as_deref() {
                        if !id.is_empty() {
                            self.sub_agent_transcripts
                                .entry(id.to_string())
                                .or_default()
                                .push(line);
                        } else {
                            self.transcript.push(line);
                        }
                    } else {
                        self.transcript.push(line);
                    }
                }
                self.note_activity(agent_id.as_deref(), Phase::ToolCall { tool: tool_name });
            }
            RuntimeEvent::ActionFinished {
                tool_name,
                summary,
                denied,
                agent_id,
                details,
                ..
            } => {
                self.track_agent(agent_id.as_deref());
                let prefix = agent_prefix(agent_id.as_deref());

                // Update SubAgentState events
                if let Some(id) = agent_id.as_deref()
                    && let Some(state) = self.sub_agents.get_mut(id)
                {
                    // Find the last unfinished event and mark it as finished
                    if let Some(event) = state.events.iter_mut().rev().find(|e| !e.is_finished) {
                        event.summary = summary.clone();
                        event.is_finished = true;
                    }
                    state.last_tool_at = Instant::now();
                }

                // Fill the invocation line's state in place (the marker slot is
                // redrawn from this) and, for a `write_file` overwrite, swap the
                // written-content block for a real old→new diff.
                let settled = if denied {
                    ToolState::Denied
                } else {
                    ToolState::Done
                };
                let is_child = matches!(agent_id.as_deref(), Some(id) if !id.is_empty());
                let adjacent = if is_child {
                    let id = agent_id.as_deref().unwrap_or_default().to_string();
                    let output = self.sub_agent_transcripts.entry(id).or_default();
                    settle_tool_call(output, settled, details.as_ref())
                } else {
                    settle_tool_call(&mut self.transcript.output, settled, details.as_ref())
                };

                // `update_plan` renders as a plan block (see `PlanUpdated`), so its
                // tool-result line is redundant — suppress it. Everything else
                // still finalizes the tool-progress state.
                if tool_name != "update_plan" {
                    let (text, kind, detail) = if denied {
                        (
                            format!("  {prefix}⛔ {tool_name} denied"),
                            LineKind::Error,
                            None,
                        )
                    } else if let Some(label) =
                        spawn_result_label(&summary).or_else(|| bg_start_result_label(&summary))
                    {
                        // A tool answers with structured JSON (a spawn, a
                        // background start). The meta row states the fact the
                        // row exists for (whom, with what capability; which
                        // task id) and the raw result rides the fold — the
                        // tool's own wording and payload stay one keypress
                        // away, and `summary` is never rewritten.
                        let body = if adjacent {
                            label
                        } else {
                            format!("{tool_name}  {label}")
                        };
                        let raw = summary.clone();
                        let detail = Some(LineDetail::Folded {
                            line_count: raw.lines().count(),
                            char_count: raw.chars().count(),
                            duration_secs: None,
                            raw,
                            // The meta row is a label, not the payload's head.
                            meta_head: MetaHead::None,
                        });
                        (format!("  {prefix}< {body}"), LineKind::ToolResult, detail)
                    } else {
                        let max_cols = self.transcript.wrap_width().saturating_sub(20).max(40);
                        // The result text is the tool's own answer and is never
                        // rewritten — only displayed. `raw` keeps it whole; the
                        // preview ladder decides how much of it to draw.
                        let raw = summary.clone();
                        let line_count = raw.lines().count();
                        let char_count = raw.chars().count();
                        // Fold only when there is something to fold: a lone
                        // short line stays a plain line, exactly as before.
                        let folds = !raw.is_empty() && (line_count > 1 || char_count > max_cols);
                        // The meta row is a *label*, not a restatement: just the
                        // answer's head line, abbreviated. The rest of the answer
                        // is payload for the preview ladder (`render.rs`), and
                        // `raw` keeps all of it — squashing the whole answer up
                        // here would leak past every tier and break the ladder.
                        let head = raw.lines().next().unwrap_or("");
                        let s = one_line(head, max_cols);
                        // The meta row *is* the answer's head (`  < head`),
                        // abbreviated to the line budget — record which, so
                        // the ladder neither draws the head twice nor loses
                        // the truncated tail at full expansion.
                        let meta_head = if s == head {
                            MetaHead::Whole
                        } else {
                            MetaHead::Abbreviated
                        };
                        // When the result sits directly under its own call the
                        // tool name is already on the line above — repeating it
                        // is chrome, not content. Out-of-order (parallel) results
                        // carry the name so the attachment stays readable.
                        let body = if s.is_empty() {
                            if adjacent {
                                String::new()
                            } else {
                                tool_name.clone()
                            }
                        } else if adjacent {
                            s
                        } else {
                            format!("{tool_name}  {s}")
                        };
                        let text = if body.is_empty() {
                            format!("  {prefix}<")
                        } else {
                            format!("  {prefix}< {body}")
                        };
                        let detail = if folds {
                            Some(LineDetail::Folded {
                                raw,
                                line_count,
                                char_count,
                                duration_secs: None,
                                meta_head,
                            })
                        } else {
                            None
                        };
                        (text, LineKind::ToolResult, detail)
                    };
                    let line = OutputLine {
                        spans: None,
                        original: None,
                        detail,
                        text,
                        kind,
                        tool_state: None,
                    };
                    // Route to the correct transcript
                    if let Some(id) = agent_id.as_deref() {
                        if !id.is_empty() {
                            self.sub_agent_transcripts
                                .entry(id.to_string())
                                .or_default()
                                .push(line);
                        } else {
                            self.transcript.push(line);
                        }
                    } else {
                        self.transcript.push(line);
                    }
                }
                // Apply real file line numbers from tool metadata (edit_file returns edit_lines)
                if let Some(ref det) = details
                    && let Some(edit_lines) = det.get("edit_lines").and_then(|v| v.as_array())
                {
                    let offsets: Vec<u32> = edit_lines
                        .iter()
                        .filter_map(|v| v.as_u64().map(|n| n as u32))
                        .collect();
                    // Find the most recent Diff in the transcript for this tool call
                    for line in self.transcript.output.iter_mut().rev() {
                        if let Some(LineDetail::Diff { hunks, .. }) = &mut line.detail {
                            for hunk in hunks.iter_mut() {
                                if let Some(&base) = offsets.get(hunk.edit_index)
                                    && base > 0
                                {
                                    let off = base - 1; // convert 1-based to offset
                                    for dl in &mut hunk.lines {
                                        if let Some(ref mut n) = dl.old_line {
                                            *n += off;
                                        }
                                        if let Some(ref mut n) = dl.new_line {
                                            *n += off;
                                        }
                                    }
                                    // Rebuild header with real line numbers
                                    let old_start =
                                        hunk.lines.iter().find_map(|l| l.old_line).unwrap_or(1);
                                    let new_start =
                                        hunk.lines.iter().find_map(|l| l.new_line).unwrap_or(1);
                                    let old_count =
                                        hunk.lines.iter().filter(|l| l.old_line.is_some()).count();
                                    let new_count =
                                        hunk.lines.iter().filter(|l| l.new_line.is_some()).count();
                                    hunk.header = format!(
                                        "@@ -{},{} +{},{} @@",
                                        old_start, old_count, new_start, new_count
                                    );
                                }
                            }
                            break; // only the most recent Diff
                        }
                    }
                }
                self.live_progress = None;
                self.note_activity(agent_id.as_deref(), Phase::Thinking);
            }
            RuntimeEvent::PlanUpdated {
                objective,
                explanation,
                plan,
                ..
            } => {
                self.flush_pending();

                // Build the new plan block: objective + status-marked steps + an
                // optional explanation. Steps are normalized to ≤60 chars by the
                // tool (fits one line); objective/explanation wrap with a hanging
                // indent.
                let mut block: Vec<OutputLine<ShellToken>> = Vec::new();
                for (i, line) in wrap(&objective, self.transcript.wrap_width())
                    .into_iter()
                    .enumerate()
                {
                    let text = if i == 0 {
                        format!("📋 {line}")
                    } else {
                        format!("   {line}")
                    };
                    block.push(OutputLine {
                        spans: None,
                        original: None,
                        detail: None,
                        text,
                        kind: LineKind::Plan,
                        tool_state: None,
                    });
                }
                for item in &plan {
                    let marker = match item.status {
                        PlanStepStatus::Completed => "✅",
                        PlanStepStatus::InProgress => "🔄",
                        PlanStepStatus::Pending => "-",
                    };
                    block.push(OutputLine {
                        spans: None,
                        original: None,
                        detail: None,
                        text: format!("   {marker} {}", item.step),
                        kind: LineKind::Plan,
                        tool_state: None,
                    });
                }
                if let Some(exp) = &explanation {
                    let exp = exp.trim();
                    if !exp.is_empty() {
                        for (i, line) in wrap(exp, self.transcript.wrap_width())
                            .into_iter()
                            .enumerate()
                        {
                            let text = if i == 0 {
                                format!("   ↳ {line}")
                            } else {
                                format!("     {line}")
                            };
                            block.push(OutputLine {
                                spans: None,
                                original: None,
                                detail: None,
                                text,
                                kind: LineKind::Plan,
                                tool_state: None,
                            });
                        }
                    }
                }

                // A plan update MOVES the block to the flow tail: the old
                // block is removed and the full plan re-appended there (the
                // tool always sends the full block), so the plan is always
                // the freshest thing in view (批4 #11). The render seam
                // absorbs the row shift via `take_index_moves`.
                self.transcript.replace_plan(block);
            }
            RuntimeEvent::AwaitingApproval { request, .. } => {
                // Log line for history; the interactive popup (5b) is driven by
                // the approval queue, which the QueuedApprovalHandler feeds.
                self.flush_pending();
                self.transcript.push(OutputLine {
                    spans: None,
                    original: None,
                    detail: None,
                    text: format!("!! approval: {}", request.title),
                    kind: LineKind::Approval,
                    tool_state: None,
                });
                self.transcript.push(OutputLine {
                    spans: None,
                    original: None,
                    detail: None,
                    text: format!("     {}", request.message),
                    kind: LineKind::Approval,
                    tool_state: None,
                });
                self.begin_activity();
                self.status = AgentStatus::Running {
                    phase: Phase::AwaitingApproval,
                };
            }
            RuntimeEvent::RunFinished { agent_id, .. } => {
                self.flush_pending();
                // Only the root agent's finish ends the turn; sub-agents report
                // their own `RunFinished` with a non-empty `agent_id`.
                match agent_id.as_deref() {
                    Some(p) if !p.is_empty() => {
                        // Child: flush its stream only. The engine's
                        // RunFinished carries NO outcome (ENG-F1 形状 B) — the
                        // row flips and the ledger marker is written when the
                        // watcher wire / registry fact lands (finalize is the
                        // single terminal site; events 只落账不判态).
                        let flushed = self.flush_child_stream(p);
                        self.sub_agent_transcripts
                            .entry(p.to_string())
                            .or_default()
                            .extend(flushed);
                    }
                    _ => {
                        // Root agent: just flush and settle. The actual
                        // outcome message ("done" / "waiting" / "error")
                        // is rendered by TurnDone / TurnError after run_turn
                        // returns. With sub-agents or background tasks still
                        // running the panel stays alive — the fan-in batch
                        // injection / background wake continues the work.
                        // `running` is NOT cleared here: settle_after_turn
                        // (TurnDone/TurnError) is the sole clearer, so a late
                        // engine event can never un-flag the next turn.
                        self.settle_status_from_inflight();
                        tracing::info!(
                            follow_bottom = self.viewport.follow_bottom,
                            scroll_offset = self.viewport.scroll_offset,
                            output_len = self.transcript.len(),
                            "run_finished: view state"
                        );
                    }
                }
            }
            RuntimeEvent::RunCancelled { agent_id, .. } => {
                self.flush_pending();
                match agent_id.as_deref() {
                    Some(p) if !p.is_empty() => {
                        // This event does know its kind: the child run was
                        // cancelled — finalize it (ledger `CANCELLED`, the
                        // — bucket; the red ✗ is for real failures only).
                        self.mark_sub_agent_finished(p, SubAgentStatus::Cancelled);
                    }
                    _ => {
                        // A rescued turn (#6) was taken back as a typing
                        // mistake — it settles quiet: no "⏹ cancelled" row,
                        // no "- cancelled" verdict. A real cancel is part of
                        // the conversation and says so.
                        if !std::mem::take(&mut self.rescued_last_turn) {
                            self.transcript.push(OutputLine {
                                spans: None,
                                original: None,
                                detail: None,
                                text: "⏹ cancelled".to_string(),
                                kind: LineKind::Cancelled,
                                tool_state: None,
                            });
                            // Mark the pending summary as cancelled so the
                            // turn-final status line reads "- cancelled"
                            // instead of "- done" when the stretch settles
                            // to Idle.
                            self.pending_cancel = true;
                        }
                        // `running` clears via the turn's terminal event
                        // (settle_after_turn) — see the RunFinished note.
                        // Cancelling the turn does not cancel in-flight work:
                        // background shell tasks keep running, so they still
                        // gate the status (Waiting) and the later wake.
                        self.settle_status_from_inflight();
                        tracing::info!(
                            follow_bottom = self.viewport.follow_bottom,
                            scroll_offset = self.viewport.scroll_offset,
                            output_len = self.transcript.len(),
                            "cancel: view state"
                        );
                    }
                }
            }
            RuntimeEvent::UserEvent { event, .. } => match event {
                // Live tool progress (e.g. streaming `execute_command` output):
                // surface the latest line in the status bar. Full output is
                // still delivered as the tool's final summary, so this is
                // feedback only — no transcript pollution.
                UserEvent::Progress { text } => {
                    self.live_progress = Some(text);
                }
                // Engine-mechanism notices (Batch E): guard degradation,
                // reaper, ... Warning is a persistent red line that must NOT
                // settle the turn (the run may still be finishing); Progress
                // rides the status bar like tool progress; Info is a plain
                // output line.
                UserEvent::Notice { kind, source, text } => match kind {
                    NoticeKind::Warning => {
                        self.flush_pending();
                        self.push_error_line(&notice_line(&source, &text));
                    }
                    NoticeKind::Progress => {
                        self.live_progress = Some(text);
                    }
                    NoticeKind::Info => {
                        self.flush_pending();
                        // Gray system line: informational, visually distinct
                        // from both tool results and errors.
                        self.transcript.push_system(&notice_line(&source, &text));
                    }
                },
                _ => {}
            },
            // Tool-call arguments still streaming in (root only). The calls
            // themselves materialize only when the turn's stream drains, so
            // this is the sole liveness signal while a long brief is being
            // written — record it and let the status strip narrate the draft
            // instead of freezing on "streaming..." (session
            // 20261006_9264ba3e: ~25 s invisible while four spawn_agent
            // tasks streamed in).
            RuntimeEvent::ActionDraft {
                index,
                name,
                args_len,
                agent_id,
                ..
            } if agent_id.as_deref().filter(|id| !id.is_empty()).is_none() => {
                self.tool_drafts.insert(index, (name, args_len));
                self.note_activity(None, Phase::Streaming);
            }
            _ => {}
        }
    }
}
