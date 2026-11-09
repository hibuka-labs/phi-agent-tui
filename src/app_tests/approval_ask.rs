//! Approval prompts and ask_user questions.

use super::super::*;
use super::common::*;
use crate::interaction::AskOption;
use crossterm::event::KeyEventKind;
use phi_agent::SessionId;

#[test]
fn approval_letters_only_select_and_enter_commits() {
    // BUG 4 (final form): letters select, bare Enter commits. A decision can
    // no longer be produced by a single keypress — see the typing-burst test.
    let mut app = App::new();
    pending_approval(&mut app);
    assert_eq!(
        app.current_approval().map(|r| r.title.as_str()),
        Some("write_file")
    );
    assert!(app.has_pending_approval());

    // Letters only select — no decision leaves the key handler.
    assert_eq!(app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE), None);
    assert_eq!(app.approval_selection, Some(ApprovalDecision::AllowOnce));
    assert_eq!(app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE), None);
    assert_eq!(app.approval_selection, Some(ApprovalDecision::AllowAlways));
    assert_eq!(app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE), None);
    assert_eq!(app.approval_selection, Some(ApprovalDecision::Deny));

    // Enter commits the selection — and only then.
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Some(Action::Approve(ApprovalDecision::Deny))
    );
    app.approve_front(ApprovalDecision::Deny);
    assert!(!app.has_pending_approval());
    assert_eq!(app.approval_selection, None);

    // Ctrl+C still cancels with a popup up (and no turn running).
    pending_approval(&mut app);
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Cancel)
    );
}

#[test]
fn approval_enter_without_selection_is_swallowed() {
    let mut app = App::new();
    pending_approval(&mut app);

    // Bare Enter with nothing selected commits nothing: it must not fall
    // through to submit, and the prompt must stay put.
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(app.has_pending_approval());

    // Esc is not a decision and not a cancel either — an IME cancel sends Esc.
    assert_eq!(app.handle_key(KeyCode::Esc, KeyModifiers::NONE), None);
    assert!(app.has_pending_approval());
    assert_eq!(app.approval_selection, None);

    // Other keys are swallowed.
    assert_eq!(app.handle_key(KeyCode::Char('x'), KeyModifiers::NONE), None);
}

#[test]
fn approval_typing_run_never_decides() {
    // BUG 4 repro (`/plugin`'s n was once read as Deny): letters inside a
    // typing run are text — every key is swallowed and no key decides.
    let mut app = App::new();
    pending_approval(&mut app);
    for c in ['/', 'p', 'l', 'u', 'g', 'i', 'n'] {
        assert_eq!(app.handle_key(KeyCode::Char(c), KeyModifiers::NONE), None);
    }
    assert!(
        app.has_pending_approval(),
        "typing must never decide a pending prompt"
    );
}

#[test]
fn approval_first_letter_of_a_typing_burst_is_not_a_decision() {
    // The live misfire (session 20261018_0affd119): typing "anyday", the `a`
    // fired AllowAlways the instant the popup opened — the first key of a
    // burst passes any isolation-gap check, so timing cannot be the gate. It
    // is now select-then-Enter: not one letter of the burst decides.
    let mut app = App::new();
    pending_approval(&mut app);
    for c in "anyday".chars() {
        assert_eq!(
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE),
            None,
            "letter {c:?} must not decide (or submit anything)"
        );
        assert!(
            app.has_pending_approval(),
            "letter {c:?} decided the prompt — the BUG 4 misfire is back"
        );
    }
    // Residual (accepted): the burst may leave the last y/a/n it typed
    // highlighted ("anyday" ends on `y` → AllowOnce selected). Committing that
    // still takes a deliberate Enter afterwards, and the highlight shows what
    // Enter would do — typing alone can no longer decide.
}

#[test]
fn approval_decision_keys_reject_chords_and_repeat() {
    let mut app = App::new();
    pending_approval(&mut app);

    // Chords never select or commit: Ctrl+A must not mean AllowAlways.
    assert_eq!(
        app.handle_key(KeyCode::Char('a'), KeyModifiers::CONTROL),
        None
    );
    assert_eq!(app.approval_selection, None);
    // Ctrl+Y is CopyLastReply in normal mode; with a popup up it is swallowed
    // rather than allowed to select AllowOnce.
    assert_eq!(
        app.handle_key(KeyCode::Char('y'), KeyModifiers::CONTROL),
        None
    );
    assert_eq!(app.approval_selection, None);
    assert!(app.has_pending_approval());

    // Auto-Repeat never selects or commits: holding `a` must not burn a queue.
    assert_eq!(
        app.handle_key_event(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat),
        None
    );
    assert_eq!(app.approval_selection, None);
    app.approval_selection = Some(ApprovalDecision::AllowAlways);
    assert_eq!(
        app.handle_key_event(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Repeat),
        None
    );
    assert!(app.has_pending_approval());
}

#[test]
fn approval_selection_resets_between_prompts() {
    // Two prompts queued: the selection made for one prompt must never commit
    // the next. After a commit, Enter alone is inert again until re-selected.
    let mut app = App::new();
    pending_approval(&mut app);
    pending_approval(&mut app);

    assert_eq!(app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE), None);
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Some(Action::Approve(ApprovalDecision::AllowOnce))
    );
    app.approve_front(ApprovalDecision::AllowOnce);
    assert!(app.has_pending_approval(), "second prompt still pending");
    assert_eq!(
        app.approval_selection, None,
        "selection must not survive a commit"
    );

    // The second prompt needs its own select-then-Enter.
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(app.has_pending_approval());
    assert_eq!(app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE), None);
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Some(Action::Approve(ApprovalDecision::Deny))
    );
}

#[test]
fn shift_tab_cycles_approval_mode() {
    let mut app = App::new();
    assert_eq!(app.approval_switch.get(), RuntimeApprovalMode::Auto);

    // Shift+Tab is a global toggle: no Action, state flips in place.
    assert_eq!(app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT), None);
    assert_eq!(app.approval_switch.get(), RuntimeApprovalMode::Ask);
    assert!(
        app.status_line().contains("Mode: ask"),
        "switching must announce the new mode on the status bar: {}",
        app.status_line()
    );

    assert_eq!(app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT), None);
    assert_eq!(app.approval_switch.get(), RuntimeApprovalMode::Auto);
}

#[test]
fn shift_tab_enters_auto_and_resolves_pending_prompts() {
    let mut app = App::new();
    app.approval_switch.set(RuntimeApprovalMode::Ask);
    pending_approval(&mut app);

    // BackTab must NOT be swallowed by the popup's y/a/n routing.
    assert_eq!(app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT), None);
    assert_eq!(app.approval_switch.get(), RuntimeApprovalMode::Auto);
    // Entering auto resolves the queued prompt — auto never blocks the run.
    assert!(!app.has_pending_approval());
}

#[test]
fn approve_front_pops_and_sends() {
    let mut app = App::new();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    app.approval_queue.push_back(ApprovalItem {
        request: ApprovalRequest {
            title: "t".to_string(),
            message: "m".to_string(),
            action_key: None,
            risk_level: phi_agent::RiskLevel::Safe,
            raw: None,
            source: None,
        },
        decision_tx: tx,
    });

    app.approve_front(ApprovalDecision::Deny);
    assert!(!app.has_pending_approval());
    assert_eq!(rx.try_recv().unwrap(), ApprovalDecision::Deny);
}

fn pending_ask(app: &mut App) -> tokio::sync::oneshot::Receiver<AskAnswer> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.ask_queue.push_back(AskItem {
        question: "先做哪个？".to_string(),
        options: vec![
            AskOption {
                id: "search".to_string(),
                label: "搜索".to_string(),
                note: Some("先打通找文件".to_string()),
            },
            AskOption {
                id: "login".to_string(),
                label: "登录".to_string(),
                note: None,
            },
        ],
        answer_tx: tx,
    });
    rx
}

#[test]
fn ask_arrows_move_the_cursor_and_enter_commits() {
    let mut app = App::new();
    let mut answer = pending_ask(&mut app);
    assert!(app.has_pending_ask());

    // ↑↓ walk the list and the trailing "Chat about this" pseudo-row; the
    // cursor starts on the first row and stops at both ends.
    assert_eq!(app.ask_selection, 0);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 1);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 2, "the pseudo-row is a stop too");
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 2, "clamped at the tail");
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 1);
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 0);
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 0, "clamped at the head");

    // Enter commits the row under the cursor — ids and labels both come back.
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(!app.has_pending_ask());
    match answer.try_recv().unwrap() {
        AskAnswer::Picked { id, label } => {
            assert_eq!(id, "login");
            assert_eq!(label, "登录");
        }
        other => panic!("expected a pick, got {other:?}"),
    }
}

#[test]
fn ask_esc_skips_and_a_skip_is_an_answer() {
    // The tool's contract: Esc → `skipped`, never a hang and never a re-ask.
    let mut app = App::new();
    let mut answer = pending_ask(&mut app);
    assert_eq!(app.handle_key(KeyCode::Esc, KeyModifiers::NONE), None);
    assert!(!app.has_pending_ask());
    assert!(matches!(answer.try_recv().unwrap(), AskAnswer::Skipped));
}

/// protects: the "Chat about this" pseudo-row as the third exit — Enter on it
/// opens the clarify input, and Enter inside sends the typed text verbatim as
/// the answer and dequeues the question.
/// fails_when: Enter on the pseudo-row picks/skips instead of clarifying, the
/// text is trimmed or rewritten, or the prompt outlives the answer.
/// no_claim: the payload wording the model sees (phimint cards) or the
/// rendered face (render cards).
#[test]
fn the_chat_row_opens_the_clarify_input_and_sends_it() {
    let mut app = App::new();
    let mut answer = pending_ask(&mut app);
    // Walk to the pseudo-row (2 options + the tail).
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 2);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert_eq!(app.ask_clarify.as_deref(), Some(""), "clarify input open");
    assert!(app.has_pending_ask(), "typing is not an answer");

    for c in "why not both?".chars() {
        assert_eq!(app.handle_key(KeyCode::Char(c), KeyModifiers::NONE), None);
    }
    assert_eq!(app.ask_clarify.as_deref(), Some("why not both?"));
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(!app.has_pending_ask());
    assert_eq!(app.ask_clarify, None);
    assert_eq!(
        answer.try_recv().unwrap(),
        AskAnswer::Clarify {
            text: "why not both?".into()
        }
    );
}

/// protects: blank text is not an answer — Enter on an empty (or
/// whitespace-only) clarify input neither sends nor closes.
/// fails_when: an empty Enter answers, skips or drops the prompt.
/// no_claim: what non-blank text does (the send card).
#[test]
fn an_empty_clarify_enter_is_not_an_answer() {
    let mut app = App::new();
    let _answer = pending_ask(&mut app);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);

    // Empty buffer: Enter is a no-op.
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(app.has_pending_ask());
    assert_eq!(app.ask_clarify.as_deref(), Some(""), "still in the input");

    // Whitespace is blank too.
    for c in "   ".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(app.has_pending_ask());
    assert_eq!(
        app.ask_clarify.as_deref(),
        Some("   "),
        "text kept for editing"
    );
}

/// protects: the Esc round trip (clarify <-> options) discards the popup's
/// text but never the composer draft behind the modal.
/// fails_when: any clarify transition touches the composer buffer.
/// no_claim: what Esc means in list mode beyond the skip (its own card).
#[test]
fn clarify_esc_keeps_the_composer_draft() {
    let mut app = App::new();
    app.composer.insert_str("my draft");
    let mut answer = pending_ask(&mut app);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    for c in "abc".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }

    // Esc drops the popup text and lands back on the pseudo-row.
    assert_eq!(app.handle_key(KeyCode::Esc, KeyModifiers::NONE), None);
    assert_eq!(app.ask_clarify, None, "input face closed");
    assert!(app.has_pending_ask(), "Esc in clarify is not a skip");
    assert_eq!(app.ask_selection, 2, "back on the row it came from");
    assert_eq!(app.composer.text(), "my draft");

    // The second Esc is the real skip — and still leaves the draft alone.
    assert_eq!(app.handle_key(KeyCode::Esc, KeyModifiers::NONE), None);
    assert!(matches!(answer.try_recv().unwrap(), AskAnswer::Skipped));
    assert_eq!(app.composer.text(), "my draft");
}

/// protects: Ctrl+C in the clarify input is the clear gesture (composer-clear
/// semantics) — it empties the popup buffer and never cancels the turn or
/// arms a quit from behind the modal.
/// fails_when: Ctrl+C surfaces Action::Cancel/Quit here, or drops the input
/// face or the prompt.
/// no_claim: Ctrl+C behavior outside the clarify input (the global ladder).
#[test]
fn clarify_ctrl_c_clears_the_input_not_the_turn() {
    let mut app = App::new();
    let _answer = pending_ask(&mut app);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    for c in "abc".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }

    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        None,
        "Ctrl+C must not surface a cancel/quit action from the input"
    );
    assert_eq!(app.ask_clarify.as_deref(), Some(""), "buffer cleared");
    assert!(app.has_pending_ask(), "the prompt is still up");
}

/// protects: the ask popup owns paste end to end — into the clarify input it
/// flattens newlines to spaces (the box is single-line), and over the option
/// list it is swallowed instead of invisibly stomping the composer draft.
/// fails_when: a paste lands in the composer while the popup is up, or a
/// pasted newline breaks the clarify input.
/// no_claim: paste routing with mention/slash open (composer paths).
#[test]
fn the_ask_popup_owns_paste() {
    let mut app = App::new();
    app.composer.insert_str("my draft");
    let _answer = pending_ask(&mut app);

    // List mode: swallowed, draft untouched.
    app.paste("invisible stomp\n");
    assert_eq!(app.composer.text(), "my draft");

    // Clarify mode: flattened into the popup's own buffer.
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    app.paste("line1\nline2\rline3");
    assert_eq!(app.ask_clarify.as_deref(), Some("line1 line2 line3"));
    assert_eq!(app.composer.text(), "my draft");
}

/// protects: a 0-option host-call ask is answerable — the pseudo-row is its
/// only row, Enter opens the clarify input, Esc skips (no hang, no ghost).
/// fails_when: a 0-option ask has no stop to land on or no way out.
/// no_claim: the rendered window for that state (the render card).
#[test]
fn a_zero_option_ask_still_answers() {
    let mut app = App::new();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    app.ask_queue.push_back(AskItem {
        question: "free form?".to_string(),
        options: vec![],
        answer_tx: tx,
    });

    // The pseudo-row is the only stop.
    assert_eq!(app.ask_selection, 0);
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 0);
    // Enter lands on it: the clarify input.
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert_eq!(app.ask_clarify.as_deref(), Some(""));
    // Esc out, Esc again skips — the question is always answerable.
    assert_eq!(app.handle_key(KeyCode::Esc, KeyModifiers::NONE), None);
    assert_eq!(app.handle_key(KeyCode::Esc, KeyModifiers::NONE), None);
    assert!(matches!(rx.try_recv().unwrap(), AskAnswer::Skipped));
}

#[test]
fn ask_typing_is_swallowed_and_never_answers() {
    // The question owns the keyboard: a typing burst aimed at the composer
    // behind the window must not pick a row (or reach the composer at all).
    let mut app = App::new();
    let _answer = pending_ask(&mut app);
    for c in "hello".chars() {
        assert_eq!(app.handle_key(KeyCode::Char(c), KeyModifiers::NONE), None);
    }
    assert!(app.has_pending_ask(), "typing must never answer a question");
    assert_eq!(app.ask_selection, 0);
}

#[test]
fn ask_decision_keys_reject_chords_and_repeat() {
    let mut app = App::new();
    let _answer = pending_ask(&mut app);

    // Chords never move or commit: Ctrl+Enter must not answer.
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::CONTROL), None);
    assert_eq!(app.ask_selection, 0);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::CONTROL), None);
    assert!(app.has_pending_ask());

    // Auto-Repeat never moves or commits either — holding ↓ must not walk
    // the list into an accidental Enter.
    assert_eq!(
        app.handle_key_event(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Repeat),
        None
    );
    assert_eq!(app.ask_selection, 0);
}

#[test]
fn ask_cursor_resets_between_questions() {
    // Two questions queued: a cursor moved for one must never Enter-commit
    // the next one's first row without looking.
    let mut app = App::new();
    let mut first = pending_ask(&mut app);
    let mut second = pending_ask(&mut app);

    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 1);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(app.has_pending_ask(), "second question still pending");
    assert_eq!(app.ask_selection, 0, "cursor must not survive an answer");
    assert!(matches!(
        first.try_recv().unwrap(),
        AskAnswer::Picked { ref id, .. } if id == "login"
    ));

    assert_eq!(app.handle_key(KeyCode::Esc, KeyModifiers::NONE), None);
    assert!(!app.has_pending_ask());
    assert!(matches!(second.try_recv().unwrap(), AskAnswer::Skipped));
}

#[test]
fn clear_asks_answers_everything_with_no_pick() {
    // Cancel path: a question nobody is waiting on any more must not park a
    // popup over a cancelled turn. Dropping the item is the answer — the
    // tool's oneshot sees a closed channel ("skipped: the picker closed
    // without an answer", or "cancelled" when the token fired first).
    let mut app = App::new();
    let mut first = pending_ask(&mut app);
    let mut second = pending_ask(&mut app);
    app.clear_asks();
    assert!(!app.has_pending_ask());
    assert!(
        first.try_recv().is_err(),
        "dropped prompt must close its channel"
    );
    assert!(second.try_recv().is_err());
}

#[test]
fn an_approval_in_front_swallows_the_question_keys() {
    // One modal at a time: with both queued, the approval's key rules own the
    // keyboard — arrows/Enter must never answer the question behind it.
    let mut app = App::new();
    pending_approval(&mut app);
    let mut answer = pending_ask(&mut app);

    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.ask_selection, 0);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(
        app.has_pending_ask(),
        "the question behind must stay unanswered"
    );
    assert!(answer.try_recv().is_err());
}

#[test]
fn awaiting_approval_sets_status() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::AwaitingApproval {
        session_id: SessionId::new(1),
        request: ApprovalRequest {
            title: "write_file".to_string(),
            message: "Write file: src/lib.rs".to_string(),
            action_key: None,
            risk_level: phi_agent::RiskLevel::Sensitive,
            raw: None,
            source: None,
        },
        agent_id: None,
        trace_id: None,
    }));
    assert_eq!(
        app.status,
        AgentStatus::Running {
            phase: Phase::AwaitingApproval
        }
    );
}
