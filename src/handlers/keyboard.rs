//! Keyboard event handler for the TUI application.
//!
//! Handles keyboard input events, including:
//! - Global keys (Ctrl+C, Ctrl+D, etc.)
//! - Context-specific keys (approval, menu, etc.)
//! - Composer keys (typing, navigation, etc.)

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use crate::app::{Action, App};
use phi_agent::ApprovalDecision;
use phi_kernel_tools::background_shell::BackgroundTaskStatus;

impl App {
    /// Handle a keyboard event and return an action if one was triggered.
    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        self.handle_key_event(code, modifiers, KeyEventKind::Press)
    }

    /// Full keyboard entry point: `kind` separates a real press from auto-Repeat,
    /// which must never decide an approval prompt (holding `a` must not grant
    /// standing approval to a whole queue).
    pub fn handle_key_event(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        kind: KeyEventKind,
    ) -> Option<Action> {
        use KeyCode::*;

        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        let alt = modifiers.contains(KeyModifiers::ALT);
        let super_key = modifiers.contains(KeyModifiers::SUPER);

        // Any key dismisses a transient notice (copy feedback) before handling.
        // Non-Ctrl+C keys also reset the quit-hint double-press window.
        self.notice = None;
        if !(ctrl && code == Char('c')) {
            self.quit_hint_at = None;
        }

        // Cmd+C (Super) is the macOS copy shortcut: copy the selection, and do
        // nothing when there's no selection — it never cancels/quits (that's
        // Ctrl+C's job).
        if super_key && code == Char('c') {
            if self.selection_state.selection.is_some() {
                self.selection_state.context_menu = None;
                return Some(Action::CopySelection);
            }
            return None;
        }

        // While the ask clarify input is up, Ctrl+C is the composer-clear
        // gesture: it empties the popup's own buffer and nothing else — never
        // a cancel or a quit from behind the modal (Esc is the way out).
        if self.ask_clarify.is_some() && ctrl && code == Char('c') {
            self.ask_clarify_clear();
            self.quit_hint_at = None;
            return None;
        }

        // Ctrl+C: copy selection → cancel running → clear a draft → cancel
        // background tasks → double-press to quit.
        //   1. If there's an active selection, copy it (caller clears selection).
        //   2. If the agent is running (or approval pending), cancel it; a
        //      second press within the timeout force-quits (the cancel path
        //      can stall deep in the runtime — the escape hatch must not).
        //   3. If the composer holds a draft, clear it first: one press deals
        //      with the text, quitting takes an explicit second gesture. (A
        //      running turn keeps cancel at priority 2, so a typed draft
        //      survives the cancel and is cleared by the next press.)
        //   4. If there are running background tasks, cancel them.
        //   5. Otherwise, show a hint; a second Ctrl+C within timeout quits.
        if ctrl && code == Char('c') {
            const DOUBLE_PRESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
            if self.selection_state.selection.is_some() {
                tracing::info!("ctrl+c: has selection -> copy");
                self.selection_state.context_menu = None;
                self.quit_hint_at = None;
                return Some(Action::CopySelection);
            }
            if self.running || !self.approval_queue.is_empty() || !self.pick_queue.is_empty() {
                // A cancel was already requested recently and the agent is
                // still stuck: force-quit instead of sending a no-op cancel.
                if let Some(at) = self.quit_hint_at
                    && at.elapsed() < DOUBLE_PRESS_TIMEOUT
                {
                    tracing::info!("ctrl+c: stuck after cancel -> force quit");
                    self.quit_hint_at = None;
                    return Some(Action::Quit);
                }
                tracing::info!("ctrl+c: running/approval -> cancel");
                self.quit_hint_at = Some(Instant::now());
                self.set_notice("Cancelling... press Ctrl+C again to force quit");
                return Some(Action::Cancel);
            }
            // A draft is the user's own typing: clear it before any quit
            // gesture, and start a fresh gesture window (clearing is the
            // complete action; it must not drift into a quit).
            if !self.composer.text().is_empty() {
                tracing::info!("ctrl+c: draft -> clear composer");
                self.composer.clear();
                self.quit_hint_at = None;
                return None;
            }
            // Cancel running background tasks
            let running_bg_tasks: Vec<String> = self
                .background_tasks
                .iter()
                .filter(|(_, t)| t.status == BackgroundTaskStatus::Running)
                .map(|(id, _)| id.clone())
                .collect();
            if !running_bg_tasks.is_empty() {
                tracing::info!(task_ids = ?running_bg_tasks, "ctrl+c: cancel background tasks");
                for task_id in &running_bg_tasks {
                    if let Some(registry) = &self.background_registry {
                        registry.cancel(task_id);
                    }
                }
                self.set_notice("Background tasks cancelled");
                return None;
            }
            // Double-press to quit: first press shows hint, second press
            // within timeout actually quits.
            if let Some(at) = self.quit_hint_at
                && at.elapsed() < DOUBLE_PRESS_TIMEOUT
            {
                tracing::info!("ctrl+c: hint active -> quit");
                self.quit_hint_at = None;
                return Some(Action::Quit);
            }
            tracing::info!("ctrl+c: idle -> show quit hint");
            self.quit_hint_at = Some(Instant::now());
            self.set_notice("Press Ctrl+C again to exit");
            return None;
        }

        // Shift+Tab: cycle the approval mode (auto ⇄ ask). Global — works even
        // while an approval popup is up (entering auto resolves pending
        // prompts; see App::cycle_approval_mode). `BackTab` is what crossterm
        // reports for Shift+Tab.
        if code == BackTab {
            let mode = self.cycle_approval_mode();
            tracing::info!(mode = ?mode, "shift+tab: approval mode cycled");
            return None;
        }

        // Ctrl+L: force a full screen repaint. Global — it is the user's
        // escape hatch for terminal-grid desync (clipped cells after a size
        // pulse, stale composite rows), so it must fire even while a popup
        // would otherwise swallow keys. Plain 'l' still types a literal 'l'.
        if ctrl && code == Char('l') {
            return Some(Action::Redraw);
        }

        // Product panel recovery chord (keymap, default Ctrl+B): reopen the
        // design canvas (1C row 16). Global for the same reason as Ctrl+L —
        // the crash face advertises "ctrl+b reopen", and an approval popup (a
        // canvas consent prompt is exactly the one that is up when the server
        // dies) must not swallow the recovery key it advertises. A chord can
        // never be an approval decision.
        if ctrl && code == Char(self.keymap.canvas_reopen) {
            return Some(Action::CanvasReopen);
        }

        // Right-click copy menu: Up/Down move the highlight, Enter copies (or
        // cancels), Esc closes; everything else is swallowed.
        if self.selection_state.menu_is_open() {
            let selected = self.selection_state.menu_selected();
            return match code {
                Up => {
                    self.selection_state.menu_move(-1);
                    None
                }
                Down => {
                    self.selection_state.menu_move(1);
                    None
                }
                Enter => {
                    self.selection_state.context_menu = None;
                    if selected == 0 {
                        Some(Action::CopySelection)
                    } else {
                        None
                    }
                }
                Esc => {
                    self.selection_state.context_menu = None;
                    None
                }
                _ => None,
            };
        }

        // While an approval popup is showing, letters only SELECT a choice and
        // bare Enter COMMITS it (Claude Code's modal pattern).
        // BUG 4 (final form): timing cannot tell a lone decision key from the
        // first letter of a typing burst ("anyday" — its `a` once granted
        // AllowAlways). So a decision needs a commit step typing never produces:
        //  - `y`/`a`/`n` (Press, no chord) only set `approval_selection`;
        //  - bare Enter submits the selection; with nothing selected it is a
        //    no-op;
        //  - everything else is swallowed — including Esc (an IME cancel sends
        //    Esc), chords (Ctrl+A must never mean AllowAlways) and auto-Repeat
        //    (holding `a` must not grant standing approval to a whole queue).
        if !self.approval_queue.is_empty() {
            let bare = !modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
            // Consent-gate prompts (product `is_consent`) offer no standing
            // consent: `a` is not in their legend, so it aliases to the
            // one-shot grant rather than AllowAlways (which nothing would
            // honor as standing anyway).
            let consent = self
                .approval_queue
                .front()
                .is_some_and(|item| self.is_consent_request(&item.request));
            return match code {
                Char(c) if kind == KeyEventKind::Press && bare => {
                    let choice = match c.to_ascii_lowercase() {
                        'y' => Some(ApprovalDecision::AllowOnce),
                        'a' if consent => Some(ApprovalDecision::AllowOnce),
                        'a' => Some(ApprovalDecision::AllowAlways),
                        'n' => Some(ApprovalDecision::Deny),
                        _ => None,
                    };
                    if choice.is_some() {
                        self.approval_selection = choice;
                    }
                    None
                }
                Enter if kind == KeyEventKind::Press && bare => {
                    self.approval_selection.clone().map(Action::Approve)
                }
                _ => None,
            };
        }

        // While an ask_user popup is showing, the popup owns the keyboard:
        // ↑↓ move the cursor (the trailing "Chat about this" pseudo-row is a
        // stop), Enter commits the row under it, Esc skips (a skip is an
        // answer too — the tool's contract says so). Enter on the pseudo-row
        // swaps in the clarify input, whose own buffer owns every key until
        // Enter sends the text or Esc backs out to the list. Everything else
        // is swallowed so it never lands in the composer behind the popup.
        if !self.ask_queue.is_empty() {
            let bare = !modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
            if self.ask_clarify.is_some() {
                return match code {
                    Enter if kind == KeyEventKind::Press && bare => {
                        self.answer_ask_clarify();
                        None
                    }
                    Esc if kind == KeyEventKind::Press && bare => {
                        self.ask_clarify_cancel();
                        None
                    }
                    Backspace if kind == KeyEventKind::Press && bare => {
                        self.ask_clarify_backspace();
                        None
                    }
                    Char(c) if kind == KeyEventKind::Press && bare => {
                        self.ask_clarify_push(c);
                        None
                    }
                    _ => None,
                };
            }
            return match code {
                Up if kind == KeyEventKind::Press && bare => {
                    self.ask_move(-1);
                    None
                }
                Down if kind == KeyEventKind::Press && bare => {
                    self.ask_move(1);
                    None
                }
                Enter if kind == KeyEventKind::Press && bare => {
                    if self.ask_on_chat_row() {
                        self.ask_clarify_open();
                    } else {
                        self.answer_ask_front(Some(self.ask_selection));
                    }
                    None
                }
                Esc if kind == KeyEventKind::Press && bare => {
                    self.answer_ask_front(None);
                    None
                }
                _ => None,
            };
        }

        // While the product pick modal is showing, it owns the keyboard:
        // the typed filter narrows the rows, ↑↓ move the cursor, Enter
        // confirms (the typed name when nothing matches), Esc cancels.
        // Everything else is swallowed so it never lands in the composer
        // behind the modal (Ctrl+C already cancelled above).
        if !self.pick_queue.is_empty() {
            let bare = !modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
            return match code {
                Up if kind == KeyEventKind::Press && bare => {
                    self.pick_move(-1);
                    None
                }
                Down if kind == KeyEventKind::Press && bare => {
                    self.pick_move(1);
                    None
                }
                Enter if kind == KeyEventKind::Press && bare => {
                    self.pick_confirm();
                    None
                }
                Esc if kind == KeyEventKind::Press && bare => {
                    self.pick_cancel();
                    None
                }
                Backspace if kind == KeyEventKind::Press && bare => {
                    self.pick_backspace();
                    None
                }
                Char(c) if kind == KeyEventKind::Press && bare => {
                    self.pick_push_char(c);
                    None
                }
                _ => None,
            };
        }

        // While the @ mention picker is open, every key drives the picker — the
        // composer only receives the final path on Enter/Esc.
        if self.mention.is_some() {
            return self.handle_mention_key(code, modifiers);
        }

        // While the / skill picker is open, every key drives the picker.
        if self.slash.is_some() {
            return self.handle_slash_key(code, modifiers);
        }

        match code {
            // Product-semantic chords (keymap, defaults spell Ctrl+D/Y/O/E).
            Char(c) if ctrl && c == self.keymap.quit => Some(Action::Quit),
            // Copy the last reply (vim-yank convention); plain 'y' still
            // falls through to `Char(c)` and types a literal 'y'.
            Char(c) if ctrl && c == self.keymap.copy_last_reply => Some(Action::CopyLastReply),
            // Toggles committed thought blocks between the collapsed
            // one-line summary and the full dim text (global, render-time
            // decision). Key events already mark the loop dirty.
            Char(c) if ctrl && c == self.keymap.toggle_thoughts => {
                self.show_thoughts = !self.show_thoughts;
                None
            }
            // Cycles the tool-block preview ladder (compact → default →
            // expanded). Separate from the thoughts toggle so "show me the
            // thinking" keeps its own key and its own meaning; this one is
            // about how much of a tool's payload is on screen. Render-time only.
            Char(c) if ctrl && c == self.keymap.cycle_result_tier => {
                self.result_tier = self.result_tier.next();
                None
            }
            // Ctrl+T: toggle the task frame — closed opens it and enters the
            // list (D10 landing), open collapses it (single meaning, no third
            // state). Collapsing returns focus to the input box, so the main
            // output area goes back to the root transcript with it.
            Char(c) if ctrl && c == self.keymap.task_panel_toggle => {
                self.toggle_task_panel();
                None
            }
            // Ctrl+V (also Cmd+V where the terminal forwards it — most
            // macOS terminals keep Cmd+V for their own Paste): paste a
            // screenshot as a pending attachment chip. Bracketed text
            // paste is untouched (it arrives as its own event); when the
            // clipboard holds no image the handler falls back to a plain
            // text paste.
            Char('v') if ctrl || super_key => Some(Action::PasteImage),
            Esc => {
                // D8: in S2 the first Esc only drops the selection back to
                // S1 — navigation keys never touch the (invisible) draft.
                // Input's Esc keeps its meaning: clear selection, else clear
                // the composer (the draft is on screen there).
                if self.task_list_escape() {
                    return None;
                }
                if self.selection_state.selection.is_some() {
                    self.selection_state.selection = None;
                } else {
                    self.composer.clear();
                }
                None
            }
            // Typing takeover (Rev 2026-11-04, 打字即接管): typing/Enter/paste hands the
            // keyboard back to the composer first — the reply must be
            // readable on the root transcript. The pinned row demotes to a
            // corpse like any other (the sweep that reaps it is a death
            // sweep's job). ↑↓/Esc/Ctrl+T/Home/End/PgUp/PgDn never steal —
            // they are the list's own keys.
            Enter if ctrl => {
                self.steal_focus_to_composer();
                self.scroll_to_bottom();
                // Ctrl+Enter: force-send (simplified `chat:sendNow`). Queue
                // the draft and interrupt the turn so the queue goes out right
                // away (the terminal flush delivers it). With nothing to send
                // it is a no-op — never an accidental submit.
                if self.composer.is_empty() {
                    if self.running
                        && (!self.queued_steer.is_empty() || !self.queued_cmds.is_empty())
                    {
                        Some(Action::ForceSend(String::new()))
                    } else {
                        None
                    }
                } else {
                    let text = self.composer.text();
                    self.composer.clear();
                    Some(Action::ForceSend(text))
                }
            }
            Enter => {
                self.steal_focus_to_composer();
                if shift {
                    self.composer.insert_char('\n');
                    return None;
                }
                // Enter always sinks the transcript to the newest lines: the
                // key is the "back to now" gesture whether the composer
                // submits, steers, or is empty — an empty Enter used to fall
                // through to `None` and left the view wherever it had been
                // scrolled.
                self.scroll_to_bottom();
                if self.running {
                    // Never a silent drop: while the turn runs, Enter queues
                    // the message (Steer → delivered at the next tool
                    // boundary). An empty composer queues nothing.
                    if self.composer.is_empty() {
                        None
                    } else {
                        let text = self.composer.text();
                        self.composer.clear();
                        Some(Action::Steer(text))
                    }
                } else if !self.composer.is_empty() || !self.pending_images.is_empty() {
                    // A send is text, images, or both — a lone chip row
                    // submits too.
                    let text = self.composer.text();
                    self.composer.clear();
                    // Deliberately no turn state here. The echoed user message,
                    // the activity clock, the plan reset and the Thinking phase
                    // are all committed by `App::begin_user_turn` at the
                    // `Cmd::Run` send site in run.rs. `running` was already
                    // send-site-only; the rest must follow it, or a submit
                    // intercepted before the send (/resume, /upgrade) leaves
                    // the transcript echoing a message that was never sent and
                    // the status bar on a phantom "thinking" stretch (session
                    // 20261013_075bb995: a cancelled picker left total_turns
                    // at 0 while the spinner ran for minutes).
                    Some(Action::Submit(text))
                } else {
                    None
                }
            }
            // Ctrl+J: the one newline chord every terminal can send.
            // Shift+Enter only reports separately under the kitty keyboard
            // protocol; a raw-mode legacy terminal sends 0x0A for both Enter
            // chords, and crossterm maps 0x0A to Ctrl+J (it means Enter only
            // outside raw mode).
            Char('j') if ctrl => {
                self.composer.insert_char('\n');
                None
            }
            // Ctrl+H: the delete-backward chord every terminal can send.
            // A raw-mode terminal that reports the Backspace/Delete key as BS
            // (0x08 — xterm's default, and what many Linux setups send for
            // that key) reaches crossterm as `Char('h') + Ctrl`; without this
            // binding the composer TYPES a literal 'h' per press instead of
            // deleting ("hh" where the user asked for a delete).
            Char('h') if ctrl => {
                if self.composer.is_empty() {
                    self.pending_images.pop();
                } else {
                    self.composer.backspace();
                }
                None
            }
            Backspace => {
                // An empty composer backspaces the attachment chips instead
                // of nothing: the last pasted screenshot goes first.
                if self.composer.is_empty() {
                    self.pending_images.pop();
                } else {
                    self.composer.backspace();
                }
                None
            }
            Delete => {
                self.composer.delete_forward();
                None
            }
            Left => {
                self.composer.move_left();
                None
            }
            Right => {
                self.composer.move_right();
                None
            }
            // Alt+↑/↓ keep pure cursor movement — plain ↑/↓ are history's
            // (#7), so a multi-line draft still has its navigation keys.
            Up if alt => {
                self.composer.move_up();
                None
            }
            Down if alt => {
                self.composer.move_down();
                None
            }
            Up => {
                // Take back queued messages (Claude Code's "Up from the first
                // line"): undelivered steering messages return to the input
                // box, ahead of the current draft. Only messages — a held
                // command stays queued and runs at the turn end as designed.
                if self.composer.cursor().0 == 0 && !self.queued_steer.is_empty() {
                    return Some(Action::TakeBackQueued);
                }
                // The existing enter-list key: at the composer (S0/S1) it
                // opens/enters the list at the D10 landing; in S2 it moves
                // the selection up (stops at row 0).
                if self.should_show_task_panel() {
                    self.task_list_up();
                    return None;
                }
                // History recall (#7): fish-style ring whose slots include
                // the draft — ↑ past the oldest finds back what you were
                // typing. With no history the key keeps its cursor meaning.
                if !self.input_history.is_empty() {
                    let current = self.composer.text();
                    if let Some(entry) = self.input_history.up(&current) {
                        self.set_composer_text(&entry);
                        return None;
                    }
                }
                self.composer.move_up();
                None
            }
            Down => {
                // Task-list ↓ is S2-only (边界键全表): one row down, or back
                // to S1 past the full list's last row. In S0/S1 the key is
                // inert for the panel and falls through to the composer.
                if matches!(self.task_panel.focus, crate::app::FocusTarget::TaskList(_)) {
                    self.task_list_down();
                    return None;
                }
                // While a recall cycle is live, ↓ walks newer — through the
                // draft slot and around. Idle (or after an edit), the key
                // keeps its cursor meaning.
                let current = self.composer.text();
                if let Some(entry) = self.input_history.down(&current) {
                    self.set_composer_text(&entry);
                    return None;
                }
                self.composer.move_down();
                None
            }
            Home => {
                if matches!(self.task_panel.focus, crate::app::FocusTarget::TaskList(_)) {
                    self.task_list_home_end(true);
                    return None;
                }
                self.composer.move_home();
                None
            }
            End if ctrl => {
                // Jump to the newest lines: the status bar's "to bottom".
                self.scroll_to_bottom();
                None
            }
            End => {
                if matches!(self.task_panel.focus, crate::app::FocusTarget::TaskList(_)) {
                    self.task_list_home_end(false);
                    return None;
                }
                self.composer.move_end();
                None
            }
            PageUp => {
                // S2: page the selection (the window follows). Elsewhere the
                // key scrolls the transcript as always.
                if matches!(self.task_panel.focus, crate::app::FocusTarget::TaskList(_)) {
                    self.task_list_page(true);
                    return None;
                }
                self.scroll_up();
                None
            }
            PageDown => {
                if matches!(self.task_panel.focus, crate::app::FocusTarget::TaskList(_)) {
                    self.task_list_page(false);
                    return None;
                }
                self.scroll_down();
                None
            }
            Char(c)
                if c == self.keymap.slash_trigger
                    && self.composer.is_empty()
                    && !self.skill_summaries.is_empty() =>
            {
                self.steal_focus_to_composer();
                self.composer.insert_char(c);
                self.start_slash();
                None
            }
            Char(c) if c == self.keymap.mention_trigger => {
                self.steal_focus_to_composer();
                self.composer.insert_char(c);
                self.start_mention();
                None
            }
            Char(c) => {
                // A chord is not text. crossterm turns the control bytes
                // (0x01–0x1A) into `Char(letter) + Ctrl`, so an unhandled
                // Ctrl+<letter> would type a letter into the composer instead
                // of doing what the terminal means by it (Ctrl+A would type
                // 'a', Ctrl+U 'u'…). Every chord the product owns is bound
                // above; the rest are dropped, never typed.
                if modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                {
                    return None;
                }
                self.steal_focus_to_composer();
                self.composer.insert_char(c);
                None
            }
            _ => None,
        }
    }
}
