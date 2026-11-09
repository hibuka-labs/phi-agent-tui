//! Composer and viewport machinery: the steering-queue mirror, the
//! mention/slash completers, paste, and scrolling.

use super::*;

impl App {
    /// Reconcile the pending-steering mirror with the engine's queue and echo
    /// what the turn just delivered. `snapshot` is the queue's current FIFO
    /// contents; the loop drains from the front and [`Action::Steer`] appends
    /// to the back, so the mirror minus its tail is exactly what arrived at the
    /// last tool boundary — the model is about to see those, so the transcript
    /// must show them now (gray-until-started is the strip's job, not a second
    /// echo).
    ///
    /// Returns `true` when anything changed (caller sets `dirty`).
    pub(crate) fn sync_steer_queue(&mut self, snapshot: Vec<String>) -> bool {
        let delivered = self.queued_steer.len().saturating_sub(snapshot.len());
        let mut changed = false;
        let delivered_msgs: Vec<String> = self.queued_steer.drain(..delivered).collect();
        for msg in &delivered_msgs {
            self.push_user(msg);
            changed = true;
        }
        if self.queued_steer != snapshot {
            self.queued_steer = snapshot;
            changed = true;
        }
        changed
    }

    /// Hold a `/` command for the post-turn flush. Returns `false` when the
    /// same command is already waiting: a hammered key must not queue the
    /// same command twice — each held command replays as its own turn (and
    /// an interactive one like `/model` replays its whole pick + switch).
    pub(crate) fn hold_command(&mut self, text: String) -> bool {
        if self.queued_cmds.contains(&text) {
            return false;
        }
        self.queued_cmds.push(text);
        true
    }

    /// The next run the idle-frame flush should fire: undelivered steering
    /// first (merged as one turn), else the oldest held command. Exactly one
    /// run per call — the rest stay queued and ride later idle frames, so a
    /// burst of held commands can never be fired back-to-back in one pass.
    pub(crate) fn take_next_queued_run(&mut self, leftover_steer: Vec<String>) -> Option<String> {
        if !leftover_steer.is_empty() {
            return Some(leftover_steer.join("\n"));
        }
        if self.queued_cmds.is_empty() {
            return None;
        }
        Some(self.queued_cmds.remove(0))
    }

    /// Claim the mid-turn lane for this command input: `false` when the same
    /// command is already executing there. A hammered key is one intent —
    /// the repeat must not spawn a second copy of the same pick.
    pub(crate) fn claim_mid_command(&self, text: &str) -> bool {
        self.mid_commands.lock().unwrap().insert(text.to_string())
    }

    /// Put taken-back steering messages into the composer: they land above the
    /// current draft in queue order, one per line, so a re-submit sends them
    /// as one message again. Nothing was echoed yet (take-back only ever
    /// touches undelivered messages), so the transcript needs no edit.
    pub(crate) fn take_back_into_composer(&mut self, msgs: Vec<String>) {
        if msgs.is_empty() {
            return;
        }
        let existing = self.composer.text();
        self.composer.clear();
        let mut restored = msgs.join("\n");
        if !existing.is_empty() {
            restored.push('\n');
            restored.push_str(&existing);
        }
        self.composer.insert_str(&restored);
        self.queued_steer.clear();
    }

    /// Replace the composer's text with a recalled history entry (#7): the
    /// whole buffer swaps and the cursor lands at the end, ready to edit.
    pub(crate) fn set_composer_text(&mut self, text: &str) {
        self.composer.clear();
        self.composer.insert_str(text);
    }

    /// Record a sent input into the recall ring and the history file (#7).
    /// Called at every send site (submit / steer / force-send) — a command
    /// that never went out is not history.
    pub(crate) fn record_input(&mut self, text: &str) {
        self.input_history.push(text);
        if let Some(store) = &self.input_history_store {
            store.append(text);
        }
    }

    /// Arm input-history persistence (#7): `path` is the product's JSONL
    /// file (`ShellSpec::input_history`; `None` keeps the ring session-only).
    /// Loaded entries are this workspace's, oldest first.
    pub(crate) fn load_input_history(&mut self, path: Option<std::path::PathBuf>) {
        if let Some(path) = path {
            let cwd = self.workspace_root.display().to_string();
            let store = crate::input_history::InputHistoryStore::new(path, cwd);
            let entries = store.load();
            self.input_history = phi_tui::history::HistoryRing::from_entries(entries);
            self.input_history_store = Some(store);
        }
    }

    /// Rows for the queued strip above the composer: one short line per
    /// pending item (steering messages first, then held commands), capped so a
    /// queue burst can never push the composer off screen. Chrome stays
    /// CJK-width-safe: ASCII arrows and `elide`'s ASCII dots (width guard).
    pub(crate) fn queued_strip_lines(&self) -> Vec<String> {
        const MAX_ROWS: usize = 3;
        const MAX_COLS: usize = 60;
        let one_line = |s: &String| {
            let head = s.lines().next().unwrap_or("");
            let mut out = phi_tui::wrap::elide(head, MAX_COLS, phi_tui::wrap::Elide::Head);
            if s.lines().count() > 1 {
                out.push_str("...");
            }
            out
        };
        let mut rows: Vec<String> = self
            .queued_steer
            .iter()
            .map(|m| format!(" queued -> {}", one_line(m)))
            .collect();
        rows.extend(
            self.queued_cmds
                .iter()
                .map(|c| format!(" queued after turn -> {}", one_line(c))),
        );
        if rows.len() > MAX_ROWS {
            let extra = rows.len() - MAX_ROWS + 1;
            rows.truncate(MAX_ROWS - 1);
            rows.push(format!(" ... +{extra} more queued"));
        }
        rows
    }

    /// The active mention picker, if open.
    pub fn mention(&self) -> Option<&MentionCompleter> {
        self.mention.as_ref()
    }

    /// Open the picker with an empty prefix. The caller has already inserted
    /// `@` into the composer.
    pub(crate) fn start_mention(&mut self) {
        self.mention = Some(MentionCompleter::new(self.workspace_root.clone()));
    }

    /// Append a char to the typed prefix, echoing it into the composer so the
    /// picker reads as an inline filter. Newlines are dropped (paths only).
    fn mention_push_char(&mut self, c: char) {
        if c == '\n' {
            return;
        }
        self.composer.insert_char(c);
        if let Some(m) = self.mention.as_mut() {
            let _ = m.handle_key(KeyCode::Char(c));
        }
    }

    // ── Completion key skeleton (mention + slash) ──────────────────────────
    //
    // Both pickers run this one skeleton — capture the old prefix, branch on
    // the completer's answer, account the removal as
    // `1 + old_prefix.chars().count()`, then sync the composer to the prefix
    // delta on `Continue` — and differ only in their policy lines (see
    // [`CompletionPolicy`]). Characterization for this skeleton lives in the
    // `twin_snapshot_*` tests.

    /// Shared key skeleton for an open completion picker. `old_prefix` is the
    /// prefix captured before the completer saw the key; `new_prefix` is what
    /// it holds afterwards.
    fn completion_key(
        &mut self,
        policy: CompletionPolicy,
        code: KeyCode,
        old_prefix: &str,
        new_prefix: &str,
        step: CompletionStep,
    ) -> Option<Action> {
        match step {
            CompletionStep::Cancelled => {
                // Esc with a typed prefix: the text is already visible in the
                // composer, so closing the picker must not wipe it (mention
                // policy). Only a bare trigger (nothing typed yet) or
                // backspacing out removes the trigger from the input. Slash
                // wipes `trigger + prefix` on every cancel.
                let keep_text =
                    policy.esc_keeps_text && matches!(code, KeyCode::Esc) && !old_prefix.is_empty();
                if !keep_text {
                    self.backspace_chars(1 + old_prefix.chars().count());
                }
                self.close_completion();
                None
            }
            CompletionStep::Selected(text) => {
                // Delete `trigger + prefix`, insert the policy-chosen text.
                self.backspace_chars(1 + old_prefix.chars().count());
                self.close_completion();
                self.composer.insert_str(&text);
                None
            }
            CompletionStep::Continue => {
                // Prefix changed (char typed, backspace, or directory
                // navigation): the composer is the display source of truth,
                // so replace the old prefix with the new one verbatim.
                if new_prefix != old_prefix {
                    self.backspace_chars(old_prefix.chars().count());
                    self.composer.insert_str(new_prefix);
                }
                None
            }
            CompletionStep::Unhandled => None,
        }
    }

    /// Close whichever completion picker is open. At most one is (key routing
    /// checks mention first), so clearing both is equivalent to closing it.
    fn close_completion(&mut self) {
        self.mention = None;
        self.slash = None;
    }

    /// Remove the last `n` characters from the composer. The completion
    /// accounting unit is chars (not bytes), matching the prefix's own measure
    /// — multi-byte prefixes must not over-delete.
    fn backspace_chars(&mut self, n: usize) {
        for _ in 0..n {
            self.composer.backspace();
        }
    }

    /// Key handling while the mention picker is open (swallows everything).
    /// The skeleton is [`App::completion_key`]; the policy lines are the
    /// selection format and [`CompletionPolicy::MENTION`].
    pub(crate) fn handle_mention_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<Action> {
        let _ = modifiers;
        let m = self.mention.as_mut()?;
        let old_prefix = m.prefix().to_string();
        let step = match m.handle_key(code) {
            CompleterAction::Cancelled => CompletionStep::Cancelled,
            // Mention policy line: a selection inserts the finished path.
            CompleterAction::Selected(_) => CompletionStep::Selected(m.finish_text()),
            CompleterAction::Continue => CompletionStep::Continue,
            CompleterAction::Unhandled(_) => CompletionStep::Unhandled,
        };
        let new_prefix = m.prefix().to_string();
        self.completion_key(
            CompletionPolicy::MENTION,
            code,
            &old_prefix,
            &new_prefix,
            step,
        )
    }

    /// The active skill picker, if open.
    pub fn slash(&self) -> Option<&SlashCompleter> {
        self.slash.as_ref()
    }

    /// Open the skill picker (triggered when `/` is typed at line start).
    pub(crate) fn start_slash(&mut self) {
        self.slash = Some(SlashCompleter::new(self.skill_summaries.clone()));
    }

    /// Push a character into the slash prefix.
    fn slash_push_char(&mut self, c: char) {
        if let Some(s) = self.slash.as_mut() {
            let _ = s.handle_key(KeyCode::Char(c));
        }
        self.composer.insert_char(c);
    }

    // ── / skill picker ──────────────────────────────────────────────────────

    /// Key handling while the skill picker is open. The skeleton is
    /// [`App::completion_key`]; the policy lines are the empty-match Enter
    /// pre-check below, the selection format, and [`CompletionPolicy::SLASH`].
    pub(crate) fn handle_slash_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<Action> {
        let _ = modifiers;

        // Slash policy line: Enter with no matching skill is a real command
        // with arguments (`/tools off <key>`, `/connect <name>`, `/plugin add
        // <path>`), not a completion target. Close the picker and submit the
        // text as typed. The completer's Confirm-with-empty-selection
        // degrades to Cancelled, whose wipe-the-line semantics silently
        // deleted the whole command before it ever reached `parse_command`
        // (observed in session 20261015_35ab9031: every `/tools off add_tool`
        // vanished without a trace while the agent was idle). While a turn
        // runs the line just waits in the composer — same no-submit criterion
        // as a plain Enter.
        if code == KeyCode::Enter && self.slash.as_ref().is_some_and(|s| s.entries_count() == 0) {
            self.close_completion();
            if !self.running && !self.composer.is_empty() {
                let text = self.composer.text();
                self.composer.clear();
                return Some(Action::Submit(text));
            }
            return None;
        }

        let s = self.slash.as_mut()?;
        let old_prefix = s.prefix().to_string();
        let step = match s.handle_key(code) {
            CompleterAction::Cancelled => CompletionStep::Cancelled,
            // Slash policy line: a selection inserts `/{name} `.
            CompleterAction::Selected((name, _)) => CompletionStep::Selected(format!("/{name} ")),
            CompleterAction::Continue => CompletionStep::Continue,
            CompleterAction::Unhandled(_) => CompletionStep::Unhandled,
        };
        let new_prefix = s.prefix().to_string();
        self.completion_key(
            CompletionPolicy::SLASH,
            code,
            &old_prefix,
            &new_prefix,
            step,
        )
    }

    /// Paste text into the composer — or into the mention/slash prefix when a
    /// picker is open (newlines are dropped from a path prefix). Pasting is
    /// typing (Rev 2026-11-04, typing takeover 打字即接管): it hands the keyboard back to the
    /// composer first, walking away from a pinned task row.
    ///
    /// The ask popup owns input end to end: a paste into the clarify input
    /// goes to its own buffer (single line — newlines flatten to spaces), and
    /// a paste over the option list is swallowed like every other key, never
    /// an invisible stomp on the composer draft behind the modal. The pick
    /// modal's filter is the same kind of own buffer (newlines dropped).
    pub fn paste(&mut self, text: &str) {
        if self.ask_clarify.is_some() {
            for c in text.chars() {
                self.ask_clarify_push(c);
            }
            return;
        }
        if !self.ask_queue.is_empty() {
            return;
        }
        if !self.pick_queue.is_empty() {
            for c in text.chars() {
                self.pick_push_char(c);
            }
            return;
        }
        self.steal_focus_to_composer();
        if self.mention.is_some() {
            for c in text.chars() {
                self.mention_push_char(c);
            }
        } else if self.slash.is_some() {
            for c in text.chars() {
                self.slash_push_char(c);
            }
        } else {
            self.composer.insert_str(text);
        }
    }

    /// Scroll up by half a screen (one PageUp press). Returns `true` if the
    /// viewport moved.
    pub(crate) fn scroll_up(&mut self) -> bool {
        self.viewport.scroll_up(self.viewport.page_step())
    }

    /// Scroll down by half a screen (one PageDown press). Returns `true` if
    /// the viewport moved.
    pub(crate) fn scroll_down(&mut self) -> bool {
        self.viewport.scroll_down(self.viewport.page_step())
    }

    /// Snap back to the newest lines (re-enter follow-bottom). Used on
    /// submit so the reply streams into view.
    pub(crate) fn scroll_to_bottom(&mut self) {
        self.viewport.scroll_to_bottom();
    }

    /// Scroll up by a few lines (one wheel tick). A trackpad swipe delivers
    /// a rapid stream of wheel events, so a small step composes into smooth
    /// motion; the half-page step stays reserved for PgUp/PgDn presses.
    pub(crate) fn scroll_wheel_up(&mut self) -> bool {
        self.viewport.scroll_up(WHEEL_STEP)
    }

    /// Scroll down by a few lines (one wheel tick).
    pub(crate) fn scroll_wheel_down(&mut self) -> bool {
        self.viewport.scroll_down(WHEEL_STEP)
    }
}
