//! Approval and ask popups: the front-of-queue state machines.

use super::*;

impl App {
    // `status_line` / `status_fit` / `draft_status_text` live in
    // `task_panel.rs`: the process row now composes a lead + the task
    // segment through the task_layout squeeze ladder.

    /// The request currently shown by the approval popup (front of the queue).
    pub fn current_approval(&self) -> Option<&ApprovalRequest> {
        self.approval_queue.front().map(|item| &item.request)
    }

    /// Mount the product's consent-gate classifier (ShellSpec `is_consent`).
    pub fn set_is_consent(&mut self, probe: ConsentProbe) {
        self.is_consent = Some(probe);
    }

    /// Whether an approval request is a product consent gate (two-key legend;
    /// `a` aliases the one-shot grant) rather than a tool approval.
    pub(crate) fn is_consent_request(&self, request: &ApprovalRequest) -> bool {
        self.is_consent.as_ref().is_some_and(|probe| probe(request))
    }

    /// Whether an approval popup is pending.
    pub fn has_pending_approval(&self) -> bool {
        !self.approval_queue.is_empty()
    }

    /// Resolve the front approval with `decision` (popping it off the queue).
    /// The selection never leaks to the next prompt in the queue.
    pub fn approve_front(&mut self, decision: ApprovalDecision) {
        self.approval_selection = None;
        if let Some(item) = self.approval_queue.pop_front() {
            let _ = item.decision_tx.send(decision);
        }
    }

    /// Dismiss every pending approval without deciding (cancel path). The
    /// selection goes with them — a stale highlight must never Enter-commit a
    /// later prompt.
    pub fn clear_approvals(&mut self) {
        self.approval_selection = None;
        self.approval_queue.clear();
    }

    /// The question currently shown by the ask popup (front of the queue).
    pub fn current_ask(&self) -> Option<&AskItem> {
        self.ask_queue.front()
    }

    /// Whether an ask popup is pending.
    pub fn has_pending_ask(&self) -> bool {
        !self.ask_queue.is_empty()
    }

    /// Move the ask cursor by `delta` rows: the option list plus the trailing
    /// "Chat about this" pseudo-row (`options.len() + 1` rows, always at least
    /// the pseudo-row itself so a 0-option ask still has a stop).
    pub fn ask_move(&mut self, delta: isize) {
        let Some(len) = self.ask_queue.front().map(|item| item.options.len()) else {
            return;
        };
        let rows = len + 1;
        let current = self.ask_selection.min(rows - 1);
        self.ask_selection = (current as isize + delta).clamp(0, rows as isize - 1) as usize;
    }

    /// Whether the cursor sits on the "Chat about this" pseudo-row (the tail).
    pub(crate) fn ask_on_chat_row(&self) -> bool {
        self.ask_queue
            .front()
            .is_some_and(|item| self.ask_selection >= item.options.len())
    }

    /// Enter the clarify input: the popup swaps the option list for a one-line
    /// question box backed by its own buffer (the composer draft is never
    /// touched — entering and leaving must not lose it).
    pub fn ask_clarify_open(&mut self) {
        self.ask_clarify = Some(String::new());
    }

    /// Leave the clarify input for the option list, discarding the text.
    pub fn ask_clarify_cancel(&mut self) {
        self.ask_clarify = None;
    }

    /// Type into the clarify input. Single line: a newline can only arrive
    /// from a paste, and it flattens to a space instead of breaking the box.
    pub fn ask_clarify_push(&mut self, c: char) {
        if let Some(buffer) = &mut self.ask_clarify {
            buffer.push(if c == '\n' || c == '\r' { ' ' } else { c });
        }
    }

    /// Backspace in the clarify input.
    pub fn ask_clarify_backspace(&mut self) {
        if let Some(buffer) = &mut self.ask_clarify {
            buffer.pop();
        }
    }

    /// Clear the clarify input (Ctrl+C), keeping the input face up.
    pub fn ask_clarify_clear(&mut self) {
        if let Some(buffer) = &mut self.ask_clarify {
            buffer.clear();
        }
    }

    /// Send the clarify input as the answer to the front ask (a `Clarify`
    /// carrying the text verbatim) and dequeue it. Blank text is not an
    /// answer: nothing is sent, the prompt stays up (returns `false`).
    pub fn answer_ask_clarify(&mut self) -> bool {
        let Some(text) = self.ask_clarify.clone() else {
            return false;
        };
        if text.trim().is_empty() {
            return false;
        }
        self.ask_clarify = None;
        self.ask_selection = 0;
        if let Some(item) = self.ask_queue.pop_front() {
            let _ = item.answer_tx.send(AskAnswer::Clarify { text });
        }
        true
    }

    /// Answer the front ask prompt: `Some(index)` commits that row, `None`
    /// skips. The cursor never leaks to the next prompt in the queue.
    pub fn answer_ask_front(&mut self, picked: Option<usize>) {
        self.ask_selection = 0;
        self.ask_clarify = None;
        if let Some(item) = self.ask_queue.pop_front() {
            let answer = match picked.and_then(|index| item.options.get(index)) {
                Some(option) => AskAnswer::Picked {
                    id: option.id.clone(),
                    label: option.label.clone(),
                },
                None => AskAnswer::Skipped,
            };
            let _ = item.answer_tx.send(answer);
        }
    }

    /// Dismiss every pending question without answering (cancel path).
    /// Dropping the item answers the tool right away — the oneshot sees a
    /// closed channel instead of a question parked over a cancelled turn.
    pub fn clear_asks(&mut self) {
        self.ask_selection = 0;
        self.ask_clarify = None;
        self.ask_queue.clear();
    }

    // ── Product pick modal (the `/model` chooser) ─────────────────────────

    /// The pick item currently shown (front of the queue).
    pub fn current_pick(&self) -> Option<&PickItem> {
        self.pick_queue.front()
    }

    /// Whether a pick modal is pending.
    pub fn has_pending_pick(&self) -> bool {
        !self.pick_queue.is_empty()
    }

    /// Queue a product pick; the first one arms its filter state now.
    pub fn push_pick(&mut self, item: PickItem) {
        self.pick_queue.push_back(item);
        if self.pick_filter.is_none() {
            self.reset_pick_filter();
        }
    }

    /// (Re)build the filter state for the front item: every row visible, the
    /// cursor on the first, no prefix typed yet.
    fn reset_pick_filter(&mut self) {
        let rows = self
            .pick_queue
            .front()
            .map(|item| item.rows.len())
            .unwrap_or(0);
        let mut picker = phi_tui::picker::Picker::<usize>::new();
        picker.entries = (0..rows).collect();
        self.pick_filter = Some(picker);
    }

    /// Refilter the row indices against the typed prefix (case-insensitive
    /// substring on the label) and clamp the cursor into the new list.
    fn refilter_pick(&mut self) {
        let Some(item) = self.pick_queue.front() else {
            return;
        };
        let Some(picker) = self.pick_filter.as_mut() else {
            return;
        };
        let prefix = picker.prefix.to_lowercase();
        picker.entries = item
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.label.to_lowercase().contains(&prefix))
            .map(|(index, _)| index)
            .collect();
        picker.clamp_selection();
    }

    /// Type into the filter. Newlines only arrive from a paste — dropped so
    /// the filter stays a single line.
    pub fn pick_push_char(&mut self, c: char) {
        if c == '\n' || c == '\r' {
            return;
        }
        if let Some(picker) = self.pick_filter.as_mut() {
            picker.push_char(c);
        }
        self.refilter_pick();
    }

    /// Backspace the last filter char (an empty filter is a no-op — Esc owns
    /// the way out, unlike the inline `@` picker whose trigger must go).
    pub fn pick_backspace(&mut self) {
        if let Some(picker) = self.pick_filter.as_mut() {
            picker.pop_char();
        }
        self.refilter_pick();
    }

    /// Move the pick cursor by `delta` rows (clamped into the filtered list).
    pub fn pick_move(&mut self, delta: i32) {
        if let Some(picker) = self.pick_filter.as_mut() {
            picker.move_selection(delta);
        }
    }

    /// Enter on the pick modal: confirm the row under the cursor. When the
    /// filter matches no row, the typed name is the answer as typed (the
    /// literal fallback — "set it anyway"). A blank name is not an answer:
    /// the modal stays up.
    pub fn pick_confirm(&mut self) {
        let Some(item) = self.pick_queue.front() else {
            return;
        };
        let Some(picker) = self.pick_filter.as_ref() else {
            return;
        };
        let answer = match picker
            .entries
            .get(picker.selected)
            .copied()
            .and_then(|index| item.rows.get(index))
        {
            Some(row) => Some(PickAnswer::Chosen {
                id: row.id.clone(),
                literal: false,
            }),
            None => {
                let typed = picker.prefix.trim();
                (!typed.is_empty()).then(|| PickAnswer::Chosen {
                    id: typed.to_string(),
                    literal: true,
                })
            }
        };
        if let Some(answer) = answer {
            self.advance_pick_front(answer);
        }
    }

    /// Esc on the pick modal: an explicit cancel — nothing changes.
    pub fn pick_cancel(&mut self) {
        self.advance_pick_front(PickAnswer::Cancelled);
    }

    /// Dequeue the front pick with `answer` and arm the filter for the next
    /// queued item (a fresh modal, never a stale cursor).
    fn advance_pick_front(&mut self, answer: PickAnswer) {
        self.pick_filter = None;
        if let Some(item) = self.pick_queue.pop_front() {
            let _ = item.answer_tx.send(answer);
        }
        if !self.pick_queue.is_empty() {
            self.reset_pick_filter();
        }
    }

    /// Dismiss every pending pick without confirming (cancel path): each
    /// awaiting command is answered `Cancelled` — never left parked on a
    /// popup over a cancelled turn.
    pub fn clear_picks(&mut self) {
        self.pick_filter = None;
        while let Some(item) = self.pick_queue.pop_front() {
            let _ = item.answer_tx.send(PickAnswer::Cancelled);
        }
    }
}
