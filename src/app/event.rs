//! Event dispatch, turn settlement, and the activity clock.

use super::*;

impl App {
    // ── Event handling ────────────────────────────────────────────────────

    pub fn handle_event(&mut self, ev: TuiEvent) {
        match ev {
            TuiEvent::Runtime(ev) => self.handle_runtime(ev),
            TuiEvent::TurnError(msg) => {
                self.flush_pending();
                self.push_error_line(&msg);
                self.settle_after_turn(TurnEnd::Error);
            }
            TuiEvent::Warning(msg) => {
                self.flush_pending();
                self.push_error_line(&msg);
            }
            TuiEvent::Lifecycle(snap) => self.apply_lifecycle_snapshot(&snap),
            TuiEvent::TurnDone => {
                self.flush_pending();
                self.settle_after_turn(TurnEnd::Done);
            }
            TuiEvent::TurnCancelled => {
                self.flush_pending();
                // Verdict determinism: the summary snapshot must read
                // "cancelled" whichever lands first — this settle or the
                // engine's `RunCancelled` (its bus hop can lose the race).
                // The "⏹ cancelled" marker row stays with `RunCancelled`:
                // it owns the rescue gate consumption — a rescued turn stays
                // silent here too (no verdict stamp), and the flag is only
                // read, never taken, so the late engine event still settles
                // quiet on it.
                if !self.rescued_last_turn {
                    self.pending_cancel = true;
                }
                self.settle_after_turn(TurnEnd::Done);
            }
            TuiEvent::SystemMessage(msg) => {
                self.flush_pending();
                self.push_system(&msg);
                self.settle_after_turn(TurnEnd::Command);
            }
            TuiEvent::SystemLine(msg) => {
                // The mid-turn lane's answer: a system line while the turn
                // keeps streaming — no settle, no verdict line.
                self.flush_pending();
                self.push_system(&msg);
            }
            TuiEvent::StatusHint(hint) => {
                // Commit it to the transcript as well: the status slot lives on
                // the alt-screen and dies with it, so a quick start/exit left
                // the user with no trace of the notice (session
                // 20261018_7844215b). The committed transcript is what the exit
                // replay reprints into scrollback.
                self.flush_pending();
                self.push_system(&hint);
                self.upgrade_hint = Some(hint);
                tracing::info!("status hint set");
            }
            TuiEvent::TipsUpdated { items } => {
                self.set_tips(items);
            }
            TuiEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                total_tokens,
            } => {
                self.tokens_in += input_tokens;
                self.tokens_out += output_tokens;
                self.turn_tokens += input_tokens + output_tokens;
                // Window = what the provider measured this call. Prefer its
                // own total: on the OpenAI protocol `prompt_tokens` already
                // includes the cached slice, so field-summing would count it
                // twice; a provider without totals is the Anthropic shape,
                // where the fields are disjoint and their sum is exact.
                self.window_tokens = total_tokens.unwrap_or_else(|| {
                    input_tokens + cache_read_tokens.unwrap_or(0) + output_tokens
                });
            }
            // T12: degrade reports are consumed by the run loop (it owns the
            // frame flipbook mark); a direct feed here only logs.
            TuiEvent::Degrade(name) => {
                tracing::error!(hook = name, "product hook degraded to fallback");
            }
        }
    }

    /// Red error line for turn errors and mid-turn warnings (wrapped).
    /// Styled via `LineKind::Error` — no emoji prefix, the color carries the
    /// severity.
    pub(crate) fn push_error_line(&mut self, msg: &str) {
        let err_text = msg.to_string();
        for (i, line) in wrap(&err_text, self.transcript.wrap_width())
            .into_iter()
            .enumerate()
        {
            self.transcript.push(OutputLine {
                spans: None,
                original: if i == 0 { Some(err_text.clone()) } else { None },
                detail: None,
                text: line,
                kind: LineKind::Error,
                tool_state: None,
            });
        }
    }

    /// Amber attention line for engine notices (wrapped).
    ///
    /// The counterpart of [`Self::push_error_line`]: a warning that means
    /// "look at this" wears its own face, so a red line keeps meaning
    /// "something broke".
    pub(crate) fn push_notice_line(&mut self, msg: &str) {
        let text = msg.to_string();
        for (i, line) in wrap(&text, self.transcript.wrap_width())
            .into_iter()
            .enumerate()
        {
            self.transcript.push(OutputLine {
                spans: None,
                original: if i == 0 { Some(text.clone()) } else { None },
                detail: None,
                text: line,
                kind: LineKind::Notice,
                tool_state: None,
            });
        }
    }

    /// A settled-fact row in the quiet chrome voice: the turn-final verdict, a
    /// boot notice, a terminal note. The whole row carries one style run
    /// ([`ShellToken::Faint`] for quiet info, the subdued [`ShellToken::Error`]
    /// for a failed turn's marker) so it paints at the theme's chrome slot and
    /// never competes with the conversation. The row keeps `original`, so it
    /// soft-wraps at the pane width like prose — the opposite of
    /// `push_styled_line`'s fixed-width banner art, which must stay whole and
    /// clips instead.
    pub(crate) fn push_chrome_line(&mut self, text: &str, style: ShellToken) {
        self.transcript.push(OutputLine {
            spans: Some(vec![SpanSpec {
                start: 0,
                len: text.len(),
                style,
            }]),
            original: Some(text.to_string()),
            detail: None,
            text: text.to_string(),
            kind: LineKind::System,
            tool_state: None,
        });
    }

    /// Common end-of-turn settlement. If sub-agents or *bounded* background
    /// jobs are still running the task is NOT done — enter the `Waiting` state
    /// and keep the bookkeeping alive (the fan-in batch injection or the
    /// background wake starts the next turn automatically). Indefinite daemons
    /// never hold the wait: "the server is up" is steady state, not pending
    /// work, and promising a completion report for it would be a lie (the
    /// confusion this caused is session 20260922_6d262d0f).
    ///
    /// When the stretch does settle to Idle, the turn-final status line is
    /// appended to the transcript (`crunched for 1m 18s - 15.0k tok - done
    /// 20:11`, grey; `- cancelled` grey; `- error` dark red) so history shows
    /// how every turn ended. `TurnEnd::Command` settles without a line — there was no run to
    /// summarize.
    pub(crate) fn settle_after_turn(&mut self, end: TurnEnd) {
        let (_, daemons) = self.bg_running_split();
        if self.settle_status_from_inflight() {
            if let AgentStatus::Waiting { running, .. } = &self.status {
                let (bounded, _) = self.bg_running_split();
                let mut text = match (*running, bounded) {
                    (r, 0) => {
                        format!("... waiting for sub-agents ({r} running), results auto-injected")
                    }
                    (0, b) => format!(
                        "... waiting for background tasks ({b} running), auto-report on completion"
                    ),
                    (r, b) => format!(
                        "... waiting for sub-agents ({r}) + background tasks ({b}), results auto-injected"
                    ),
                };
                if daemons > 0 {
                    text.push_str(&format!(" (+{daemons} daemon(s) always-on)"));
                }
                self.transcript.push(OutputLine {
                    spans: None,
                    original: None,
                    detail: None,
                    text,
                    kind: LineKind::System,
                    tool_state: None,
                });
            }
        } else if std::mem::take(&mut self.final_status_pending) && end != TurnEnd::Command {
            // Turn-final status line: the turn's verdict travels with the
            // history instead of being overwritten on the status strip. A
            // cancelled turn reads "- cancelled" even though the settlement
            // arrives via TurnDone (RunCancelled snapshotted the summary).
            let line = self.last_turn_summary.as_ref().map(|sum| {
                let word = if sum.cancelled {
                    "cancelled"
                } else if end == TurnEnd::Error {
                    "error"
                } else {
                    "done"
                };
                // Token readout omitted when the turn never reached the model
                // (`0` usage), so command-only turns keep the bare format.
                let tok = if sum.tokens > 0 {
                    format!(" - {} tokens", fmt_k(sum.tokens))
                } else {
                    String::new()
                };
                format!(
                    "crunched for {}{} - {} {}",
                    Self::fmt_elapsed(sum.duration.as_secs()),
                    tok,
                    word,
                    sum.finished_at.format("%H:%M")
                )
            });
            if let Some(text) = line {
                // Grey verdict — one quiet-chrome run over the whole row; the
                // failure marker takes the theme's subdued `error` slot so it
                // reads as a verdict, not a live error line.
                let style = if end == TurnEnd::Error {
                    ShellToken::Error
                } else {
                    ShellToken::Faint
                };
                self.push_chrome_line(&text, style);
            }
        }
        // Daemon-only settlement: the agent IS done — say so plainly, then
        // note the service without any "waiting" framing.
        if daemons > 0 && matches!(self.status, AgentStatus::Idle) {
            self.transcript.push(OutputLine {
                spans: None,
                original: None,
                detail: None,
                text: format!(
                    "🟢 {daemons} daemon(s) running (always-on; you're notified on abnormal exit)"
                ),
                kind: LineKind::System,
                tool_state: None,
            });
        }
        self.running = false;
    }

    /// Settle the status enum from in-flight work — the single source of
    /// truth for "can the agent rest?". Sub-agents and/or *bounded* background
    /// jobs still running → `Waiting { running, bg }` (returns `true`; `bg`
    /// counts every running background task, daemons included, so the counter
    /// never under-reports). Only indefinite daemons (or nothing) → `Idle`:
    /// a server left running is the product working as intended.
    ///
    /// Every turn-end path (TurnDone/TurnError settlement, root `RunFinished`,
    /// root `RunCancelled`) must go through this so a running bounded job can
    /// never be dropped on the floor as `Idle` — the user would see "done"
    /// while the task is still working.
    pub(crate) fn settle_status_from_inflight(&mut self) -> bool {
        // Turn-end: any call still being drafted can no longer arrive.
        self.tool_drafts.clear();
        let running = self.running_sub_agents();
        let (bounded, daemons) = self.bg_running_split();
        if running > 0 || bounded > 0 {
            self.status = AgentStatus::Waiting {
                running,
                bg: bounded + daemons,
            };
            true
        } else {
            // A stretch is being settled: if one was live (activity_since set),
            // snapshot its duration + finish time for the turn-final status
            // line and arm its append (`final_status_pending`, consumed by the
            // next `settle_after_turn`). `activity_since.is_some()` both guards
            // against re-recording (settle can be called repeatedly) and
            // against recording a synthetic turn that never really ran.
            if let Some(started) = self.activity_since {
                self.last_turn_summary = Some(LastTurnSummary {
                    duration: started.elapsed(),
                    finished_at: chrono::Local::now().time(),
                    cancelled: self.pending_cancel,
                    tokens: self.turn_tokens,
                });
                self.final_status_pending = true;
                // A settled stretch advances the tips rotation (sequential
                // round-robin; the guard keeps repeated settle calls from
                // skipping entries).
                self.rotate_tip();
            }
            self.pending_cancel = false;
            self.status = AgentStatus::Idle;
            self.activity_since = None;
            // Turn-end sweep (Rev 2026-11-04, turn 收尾清场): corpses go at the root turn
            // end — the pinned row is exempt and focus is never touched.
            self.turn_end_sweep();
            false
        }
    }

    /// The live streaming tail as incrementally-wrapped lines.
    /// Returns `None` when there is no uncommitted text/thought.
    pub fn streaming_tail_lines(&self) -> Option<(&[String], LineKind)> {
        self.stream.tail_lines()
    }

    /// Raw pending text for markdown rendering (streaming tail).
    /// Returns `(text, kind)` or `None` when there is nothing pending.
    pub fn streaming_tail_raw(&self) -> Option<(&str, LineKind)> {
        self.stream.tail_raw()
    }

    /// The assistant's last reply as plain text: every committed `Normal` line
    /// since the last user turn, plus any reply still streaming. Tool calls,
    /// tool results, plan lines and the done marker are excluded — this is
    /// the prose the user most often wants to grab (e.g. a URL the AI printed).
    pub fn last_reply_text(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for line in self.transcript.output.iter().rev() {
            match line.kind {
                LineKind::User => break,
                LineKind::Normal => lines.push(line.text.clone()),
                _ => {}
            }
        }
        lines.reverse();

        // Include the uncommitted streaming tail, prefixed like `flush_text` would.
        if let Some((tail, LineKind::Normal)) = self.streaming_tail_lines() {
            let prefix = self
                .stream
                .pending_agent()
                .map(|p| format!("[{p}] "))
                .unwrap_or_default();
            for (i, line) in tail.iter().enumerate() {
                let text = if i == 0 && !prefix.is_empty() {
                    format!("{prefix}{line}")
                } else {
                    line.clone()
                };
                lines.push(text);
            }
        }

        lines.join("\n")
    }

    /// Show a transient status-bar notice (e.g. copy feedback), cleared on the
    /// next keypress.
    pub fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    /// Shift+Tab: cycle the approval mode (auto ⇄ ask; deny leaves for ask).
    ///
    /// The switch is shared with the approval gate, so the flip takes effect
    /// on the next tool call. Entering auto resolves any pending prompts
    /// (`AllowOnce`) — auto means the run is never blocked on a confirmation.
    pub fn cycle_approval_mode(&mut self) -> RuntimeApprovalMode {
        let mode = self.approval_switch.cycle_auto_ask();
        if mode == RuntimeApprovalMode::Auto {
            while self.has_pending_approval() {
                self.approve_front(ApprovalDecision::AllowOnce);
            }
        }
        // T14: the notice wording is product copy (ApprovalCopy::mode_notice).
        self.set_notice((self.copy.approval.mode_notice)(&mode));
        mode
    }

    // ── Mouse selection ───────────────────────────────────────────────────

    // ── @ mention picker ─────────────────────────────────────────────────

    /// Set the workspace root (once, at TUI start); relative paths are rendered
    /// relative to this, outside paths as absolute.
    pub fn set_workspace_root(&mut self, root: PathBuf) {
        self.workspace_root = root;
    }

    // ── / skill picker ──────────────────────────────────────────────────────

    /// Inject the loaded skill summaries (called once at startup from `run`).
    pub fn set_skill_summaries(&mut self, summaries: Vec<(String, String)>) {
        self.skill_summaries = summaries;
    }

    // ── Tips ───────────────────────────────────────────────────────────────

    /// How long an active stretch runs before the tips line appears under it
    /// (long actions get a tip too, Idle always shows one).
    const TIP_ACTIVITY_THRESHOLD: Duration = Duration::from_secs(3);

    /// Replace the tips pool (startup wiring and cloud refresh).
    pub fn set_tips(&mut self, tips: Vec<TipItem>) {
        self.tips = tips;
        self.tip_index = 0;
    }

    /// The tip shown right now (round-robin over the pool).
    pub(crate) fn current_tip(&self) -> Option<&TipItem> {
        if self.tips.is_empty() {
            return None;
        }
        self.tips.get(self.tip_index % self.tips.len())
    }

    /// Advance to the next tip (round-robin). Called once per settled turn so
    /// the next Idle stretch greets a fresh tip without repeats within a pass.
    pub(crate) fn rotate_tip(&mut self) {
        if !self.tips.is_empty() {
            self.tip_index = (self.tip_index + 1) % self.tips.len();
        }
    }

    /// Whether the tips line is on screen: always at Idle; an active
    /// Running/Waiting stretch only after [`Self::TIP_ACTIVITY_THRESHOLD`].
    pub(crate) fn tip_visible(&self) -> bool {
        if self.tips.is_empty() {
            return false;
        }
        if !self.is_active() {
            return true;
        }
        self.activity_since
            .is_some_and(|t| t.elapsed() >= Self::TIP_ACTIVITY_THRESHOLD)
    }

    /// Commit the UI state that means "a user turn is starting": echo the
    /// message into the transcript, reset the plan block, follow the bottom,
    /// start the activity clock and flip the status to Thinking.
    ///
    /// Call this at the `Cmd::Run` send site — never from key handling. Key
    /// handling only yields [`Action::Submit`]; run.rs may intercept that
    /// before the send (`/resume`, `/upgrade`), and an intercepted submit
    /// must leave nothing behind. Session 20261013_075bb995: `/resume` was
    /// submitted, the picker was cancelled, and the status bar span
    /// `thinking...` for minutes with `total_turns == 0` — the turn state had
    /// been committed by Enter with no run ever queued to settle it.
    ///
    /// The clock starts here rather than at the engine's first event so a
    /// model that thinks for half a minute before its first delta doesn't
    /// render a frozen spinner with no elapsed readout (session
    /// 20261006_9264ba3e).
    pub fn begin_user_turn(&mut self, text: &str) {
        self.begin_user_turn_with_images(text, &[]);
    }

    /// [`Self::begin_user_turn`] for a message that carries pasted screenshots:
    /// each attachment joins the echo as its own `📷 image WxH` line (inside
    /// the rescue span, so a rescued submit un-echoes them with the text).
    pub fn begin_user_turn_with_images(&mut self, text: &str, images: &[PendingImage]) {
        // A new turn supersedes anything left over from the previous one:
        // a stale rescue arm or flag must never fire into this turn.
        self.cancel_rescue = None;
        self.rescued_last_turn = false;
        self.push_user(&user_echo(text, images));
        self.transcript.clear_plan();
        // Follow the conversation: the user just committed to a turn, so the
        // reply must stream into view. If they were scrolled up reading
        // history, this is the moment their reading position ends.
        self.scroll_to_bottom();
        self.begin_activity();
        self.status = AgentStatus::Running {
            phase: Phase::Thinking,
        };
    }

    /// [`Self::begin_user_turn`] for a root user submit, plus the cancel-rescue
    /// arm (#6): a turn cancelled before it produced anything is a typing
    /// mistake, not a conversation — Ctrl+C takes the message back into the
    /// composer and un-echoes it. Synthetic turns (background wake / child
    /// fan-in) arm nothing: their echo is engine talk, never a typo.
    pub fn begin_rescuable_turn(&mut self, text: &str) {
        self.begin_rescuable_turn_with_images(text, &[]);
    }

    /// [`Self::begin_rescuable_turn`] for a submit carrying pasted screenshots:
    /// the rescue arm holds the attachments too, so a rescued submit puts the
    /// chips back instead of losing them.
    pub fn begin_rescuable_turn_with_images(&mut self, text: &str, images: &[PendingImage]) {
        let echo_start = self.transcript.output.len();
        self.begin_user_turn_with_images(text, images);
        self.cancel_rescue = Some((
            echo_start,
            self.transcript.output.len(),
            text.to_string(),
            images.to_vec(),
        ));
    }

    /// Disarm-and-check the cancel-rescue: fires only while the turn is still
    /// in flight and the model produced nothing — no committed output past
    /// the echo, no uncommitted stream tail, no tool call being drafted (CC
    /// auto-restore semantics: no text / no tool_use → take it back). On
    /// fire, the echo is rolled back, the text returns for the composer, and
    /// the phantom turn leaves no trace: the activity stretch is erased so no
    /// "crunched …" summary is ever armed for it, and the late `RunCancelled`
    /// settles quiet on `rescued_last_turn`.
    pub fn take_cancel_rescue(&mut self) -> Option<String> {
        if !self.running {
            return None;
        }
        let (echo_start, echo_end, text, images) = self.cancel_rescue.take()?;
        if self.transcript.output.len() != echo_end
            || self.stream.tail_lines().is_some()
            || !self.tool_drafts.is_empty()
        {
            // Something was produced after the echo: the turn is part of
            // the conversation now, nothing to rescue.
            return None;
        }
        self.transcript.output.truncate(echo_start);
        // The rescued submit's screenshots go back on the chip row (ahead of
        // anything pasted since) — a rescue restores the whole draft.
        self.pending_images.splice(0..0, images);
        self.activity_since = None;
        self.turn_tokens = 0;
        self.rescued_last_turn = true;
        Some(text)
    }

    /// True while a turn or a child wait is in flight — gates the event loop's
    /// liveness tick (spinner/elapsed need periodic redraws even with zero
    /// events; see `activity_since`).
    pub fn is_active(&self) -> bool {
        !matches!(self.status, AgentStatus::Idle)
    }

    /// Start the activity stretch's clock if it is not already running.
    ///
    /// The spinner and the status bar's elapsed suffix both read
    /// `activity_since`, so every path that turns the root busy must call
    /// this — the user-send shortcut, streaming events, an approval that was
    /// just granted. Idempotent on purpose: phase changes inside one stretch
    /// keep the stretch's origin (the wait after a spawn belongs to the same
    /// stretch as the send), and only [`Self::settle_status_from_inflight`]
    /// settling to Idle stops the clock.
    pub(crate) fn begin_activity(&mut self) {
        if self.activity_since.is_none() {
            self.activity_since = Some(std::time::Instant::now());
            // A brand-new stretch means the turn is leaving Idle — drop the
            // previous turn's done-summary (it is replaced by live status) and
            // disarm its final-status append in case the settlement event was
            // dropped on a dead channel (no stale line at the next turn end).
            self.last_turn_summary = None;
            self.pending_cancel = false;
            self.final_status_pending = false;
            self.turn_tokens = 0;
        }
    }

    /// Braille spinner frame for the current activity stretch (advances every
    /// 120 ms). Static when idle.
    pub(crate) fn spinner_char(&self) -> &'static str {
        const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
        let ms = self.activity_since.map_or(0, |t| t.elapsed().as_millis());
        FRAMES[(ms / 120) as usize % FRAMES.len()]
    }

    /// `9s` / `10m37s` — shared by the status-bar suffix and the thinking
    /// panel title so a long stretch reads the same in both places.
    pub(crate) fn fmt_elapsed(secs: u64) -> String {
        if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}m{:02}s", secs / 60, secs % 60)
        }
    }

    /// ` - 42s - 87.3k tokens`-style readout for the active stretch: elapsed
    /// time (rounded up so a just-started stretch doesn't read as "0s") plus
    /// the *window* — what the provider measured the context at on the last
    /// model call. The number that decides when rotation/compaction nears;
    /// the session burn lives in the exit dump. Empty when idle and no usage
    /// has arrived. ASCII separators only: `·` renders double-width in CJK
    /// fonts (render_tests CJK width guard).
    pub(crate) fn activity_suffix(&self) -> String {
        let mut suffix = match self.activity_since {
            Some(t) => format!(" - {}", Self::fmt_elapsed(t.elapsed().as_secs() + 1)),
            None => String::new(),
        };
        if self.window_tokens > 0 {
            suffix.push_str(&format!(" - {} tokens", fmt_k(self.window_tokens)));
        }
        suffix
    }
}

/// The user echo for a submit: the text, then one `📷 image WxH` line per
/// pasted screenshot (continuations of the same message, so the `❯` prefix
/// stays on the first line). An image-only submit echoes just its chips.
fn user_echo(text: &str, images: &[PendingImage]) -> String {
    let mut echo = text.to_string();
    for img in images {
        if !echo.is_empty() {
            echo.push('\n');
        }
        echo.push_str(&format!("📷 image {}x{}", img.width, img.height));
    }
    echo
}
