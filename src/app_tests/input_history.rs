//! Input-history recall (#7): the fish-style ring on ↑/↓, Alt+↑/↓ keeping
//! cursor moves, and the store roundtrip.

use super::super::*;

#[test]
fn up_recalls_and_replaces_the_composer() {
    let mut app = App::new();
    app.record_input("first");
    app.record_input("second");
    app.composer.insert_str("draft");
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "second");
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "first");
}

#[test]
fn up_cycles_back_to_the_draft_and_wraps() {
    let mut app = App::new();
    app.record_input("A");
    app.record_input("B");
    app.composer.insert_str("draft");
    // B → A → the draft is a ring slot (循环找回) → around to B again.
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "B");
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "A");
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "draft");
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "B");
}

#[test]
fn down_walks_newer_through_the_draft() {
    let mut app = App::new();
    app.record_input("A");
    app.record_input("B");
    app.composer.insert_str("draft");
    app.handle_key(KeyCode::Up, KeyModifiers::NONE); // B
    app.handle_key(KeyCode::Up, KeyModifiers::NONE); // A
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "B");
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "draft");
}

#[test]
fn down_is_inert_until_up_starts_a_cycle() {
    let mut app = App::new();
    app.record_input("A");
    app.composer.insert_str("draft");
    // No cycle live: ↓ keeps its cursor meaning (here: bottom line, no-op).
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "draft");
}

#[test]
fn editing_a_recalled_entry_breaks_the_cycle() {
    let mut app = App::new();
    app.record_input("A");
    app.record_input("B");
    app.composer.insert_str("draft");
    app.handle_key(KeyCode::Up, KeyModifiers::NONE); // B
    app.composer.insert_str(" tweaked");
    // ↓ never hijacks text the ring did not place.
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "B tweaked");
    // ↑ opens a fresh cycle around the edited text (it is the new draft).
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "B");
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "A");
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.text(), "B tweaked");
}

#[test]
fn alt_up_down_move_the_cursor_not_history() {
    let mut app = App::new();
    app.record_input("A");
    app.composer.insert_str("a\nb\nc");
    assert_eq!(app.composer.cursor(), (2, 1));
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::ALT), None);
    assert_eq!(app.composer.cursor(), (1, 1), "Alt+↑ moves the cursor");
    assert_eq!(app.composer.text(), "a\nb\nc", "and never recalls");
    assert_eq!(app.handle_key(KeyCode::Down, KeyModifiers::ALT), None);
    assert_eq!(app.composer.cursor(), (2, 1));
}

#[test]
fn empty_ring_keeps_the_cursor_keys() {
    let mut app = App::new();
    app.composer.insert_str("a\nb");
    assert_eq!(app.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app.composer.cursor().0, 0, "↑ falls back to cursor-up");
    assert_eq!(app.composer.text(), "a\nb", "nothing to recall");
}

#[test]
fn take_back_queued_keeps_its_precedence_over_recall() {
    let mut app = App::new();
    app.record_input("A");
    app.queued_steer.push("queued message".to_string());
    // Up from the first line takes the queue back before any recall.
    assert_eq!(
        app.handle_key(KeyCode::Up, KeyModifiers::NONE),
        Some(Action::TakeBackQueued)
    );
}

#[test]
fn the_store_roundtrips_across_apps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input-history.jsonl");
    let mut app = App::new();
    app.workspace_root = std::path::PathBuf::from("/proj");
    app.load_input_history(Some(path.clone()));
    app.record_input("saved one");
    app.record_input("saved two");
    // A fresh App (new session) reaches for the same inputs.
    let mut app2 = App::new();
    app2.workspace_root = std::path::PathBuf::from("/proj");
    app2.load_input_history(Some(path));
    assert_eq!(app2.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(app2.composer.text(), "saved two");
    // Another workspace does not see them.
    let mut other = App::new();
    other.workspace_root = std::path::PathBuf::from("/elsewhere");
    other.load_input_history(Some(dir.path().join("input-history.jsonl")));
    other.composer.insert_str("draft");
    assert_eq!(other.handle_key(KeyCode::Up, KeyModifiers::NONE), None);
    assert_eq!(
        other.composer.text(),
        "draft",
        "no recall from another workspace"
    );
}

#[test]
fn submit_sentinels_record_only_what_was_sent() {
    // The ring's own noise rules: blank and repeated sends are not history.
    let mut app = App::new();
    app.record_input("   ");
    app.record_input("same");
    app.record_input("same");
    assert_eq!(app.input_history.entries().len(), 1);
}
