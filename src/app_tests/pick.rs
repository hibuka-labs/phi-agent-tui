//! The product pick modal (the `/model` chooser): filter, cursor, answer.

use super::super::*;
use crate::interaction::{PickAnswer, PickItem, PickRow};

fn pick_rows(labels: &[&str]) -> Vec<PickRow> {
    labels
        .iter()
        .map(|label| PickRow {
            id: (*label).to_string(),
            label: (*label).to_string(),
            note: None,
            current: false,
        })
        .collect()
}

fn push_pick(app: &mut App, labels: &[&str]) -> tokio::sync::oneshot::Receiver<PickAnswer> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.push_pick(PickItem {
        title: "Model".to_string(),
        rows: pick_rows(labels),
        answer_tx: tx,
    });
    rx
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        assert_eq!(
            app.handle_key(KeyCode::Char(c), KeyModifiers::NONE),
            None,
            "pick keys are swallowed, never actions"
        );
    }
}

/// protects: the filter narrows the rows and Enter answers the row under the
/// cursor. fails_when: the filter shows a row it excluded, Enter answers a
/// different row than the cursor shows, or the answer does not dequeue the
/// item. no_claim: which models the product enumerates.
#[test]
fn the_filter_narrows_the_rows_and_enter_confirms_the_cursor_row() {
    let mut app = App::new();
    let mut rx = push_pick(&mut app, &["claude-opus-5-5", "deepseek-chat", "qwen-max"]);
    assert!(app.has_pending_pick());

    type_text(&mut app, "qwe");
    assert_eq!(
        app.pick_filter.as_ref().map(|p| p.entries.clone()),
        Some(vec![2]),
        "the filter must keep only matching row indices"
    );
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        None,
        "Enter answers the pick, it is not an Action"
    );
    assert!(!app.has_pending_pick(), "the answered pick is dequeued");
    assert_eq!(
        rx.try_recv().unwrap(),
        PickAnswer::Chosen {
            id: "qwen-max".into(),
            literal: false,
        }
    );
}

/// protects: the typed-name fallback — Enter on a filter that matches no row
/// takes the text as typed (`literal: true`); a blank name is not an answer
/// and the modal stays up. fails_when: Enter with no match silently closes
/// without answering, or a blank confirm changes the model. no_claim: what
/// the product does with a literal name.
#[test]
fn enter_on_no_match_takes_the_typed_name_verbatim() {
    let mut app = App::new();
    let mut rx = push_pick(&mut app, &["claude-opus-5-5"]);
    type_text(&mut app, "gpt-9");
    assert!(
        app.pick_filter
            .as_ref()
            .is_some_and(|p| p.entries.is_empty()),
        "no row matches the prefix"
    );
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert_eq!(
        rx.try_recv().unwrap(),
        PickAnswer::Chosen {
            id: "gpt-9".into(),
            literal: true,
        }
    );

    // Blank confirm is not an answer: no rows and no typed name leaves the
    // modal up and sends nothing (a literal name is only a name if there is
    // one).
    let mut rx = push_pick(&mut app, &[]);
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert!(app.has_pending_pick(), "a blank name keeps the modal up");
    assert!(rx.try_recv().is_err(), "nothing was answered");
}

/// protects: Esc is an explicit cancel that answers `Cancelled`, and the next
/// queued pick gets a fresh filter (no stale cursor or prefix). fails_when:
/// Esc closes without answering (the command action would park forever) or a
/// leftover prefix silently filters the next modal. no_claim: what the
/// product does with `Cancelled`.
#[test]
fn esc_cancels_and_the_next_pick_starts_fresh() {
    let mut app = App::new();
    let mut first = push_pick(&mut app, &["m1", "m2"]);
    let mut second = push_pick(&mut app, &["m3", "m4"]);
    type_text(&mut app, "m2");
    assert_eq!(app.handle_key(KeyCode::Esc, KeyModifiers::NONE), None);
    assert_eq!(first.try_recv().unwrap(), PickAnswer::Cancelled);

    assert!(app.has_pending_pick(), "the second pick is up");
    let picker = app.pick_filter.as_ref().expect("filter re-armed");
    assert_eq!(picker.prefix, "", "no prefix leaks into the next pick");
    assert_eq!(picker.entries, vec![0, 1], "every row visible");
    assert_eq!(app.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
    assert_eq!(
        second.try_recv().unwrap(),
        PickAnswer::Chosen {
            id: "m3".into(),
            literal: false,
        }
    );
}

/// protects: the cancel path (Ctrl+C / turn abort) answers `Cancelled` to
/// every parked command action and drains in-flight items — a command must
/// never stay parked on a popup over a cancelled turn. fails_when: a dropped
/// item leaves the action hanging on its oneshot. no_claim: the product's
/// cancel semantics beyond "nothing changed".
#[test]
fn clear_picks_answers_every_parked_pick() {
    let mut app = App::new();
    let mut first = push_pick(&mut app, &["m1"]);
    let mut second = push_pick(&mut app, &["m2"]);
    app.clear_picks();
    assert!(!app.has_pending_pick());
    assert!(app.pick_filter.is_none());
    assert_eq!(first.try_recv().unwrap(), PickAnswer::Cancelled);
    assert_eq!(second.try_recv().unwrap(), PickAnswer::Cancelled);
}

/// protects: the modal owns the keyboard — typing feeds the filter (never
/// the composer), paste flattens newlines into the single-line filter, and
/// Backspace edits the filter. fails_when: a pick keystroke lands in the
/// composer draft behind the modal. no_claim: filter matching policy beyond
/// "typed chars narrow the list".
#[test]
fn the_filter_owns_typing_and_paste_never_touches_the_composer() {
    let mut app = App::new();
    let _rx = push_pick(&mut app, &["alpha", "beta"]);
    type_text(&mut app, "alp");
    assert_eq!(
        app.composer.text(),
        "",
        "pick typing must not reach the composer"
    );
    assert_eq!(
        app.pick_filter.as_ref().map(|p| p.prefix.as_str()),
        Some("alp")
    );

    app.paste("ha\nmore");
    assert_eq!(
        app.pick_filter.as_ref().map(|p| p.prefix.as_str()),
        Some("alphamore"),
        "paste lands in the filter with newlines dropped"
    );

    for _ in 0..4 {
        app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    }
    assert_eq!(
        app.pick_filter.as_ref().map(|p| p.prefix.as_str()),
        Some("alpha")
    );
    assert_eq!(
        app.pick_filter.as_ref().map(|p| p.entries.clone()),
        Some(vec![0]),
        "backspace widens the filter again"
    );
}

/// protects: while a pick modal owns input, the process row never claims the
/// agent is thinking. The bug this pins: a `/model` queued behind a turn
/// replayed after it settled, and the row read "thinking... (Ctrl+C cancel)"
/// while the user was parked on the pick — the app looked like it was
/// answering on its own. fails_when: the row says thinking/streaming while a
/// pick is up, the pick wording outlives the pick, or an ask (which outranks
/// pick, one modal at a time) is shadowed by pick wording. no_claim: the
/// wording of any other phase.
#[test]
fn the_process_row_waits_for_the_pick_instead_of_claiming_thinking() {
    let mut app = App::new();
    app.begin_user_turn("/model");
    let before = app.status_leads();
    assert!(
        before.head.contains("thinking"),
        "the running phase reads as thinking: {}",
        before.head
    );
    assert_ne!(before.head, "waiting for your pick");

    let _rx = push_pick(&mut app, &["mimo-v2.6-pro", "MiniMax-M3.1-Flash"]);
    let parked = app.status_leads();
    assert_eq!(
        parked.head, "waiting for your pick",
        "the pick owns the next step, not the spinner"
    );
    assert!(
        parked.tail.contains("Enter set") && parked.tail.contains("Esc close"),
        "the parked row carries the modal's own key hints: {}",
        parked.tail
    );

    // An ask outranks the pick for the front surface.
    let (ask_tx, _ask_rx) = tokio::sync::oneshot::channel();
    app.ask_queue.push_back(crate::interaction::AskItem {
        question: "which one?".to_string(),
        options: vec![],
        answer_tx: ask_tx,
    });
    assert_ne!(
        app.status_leads().head,
        "waiting for your pick",
        "the ask is the front modal, not the pick"
    );
}
