//! User-message fold (#8): long input collapses to head + marker + tail.

use super::super::*;

/// A message of `n` short lines — one transcript row per line.
fn long_message(n: usize) -> String {
    (1..=n)
        .map(|i| format!("msg line {i:02}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn long_user_message_folds_to_head_marker_and_tail() {
    let mut app = App::new();
    app.push_user(&long_message(20));
    let text = snapshot_text(&mut app, 80, 40);
    // Head rows (1..=8) and tail rows (19..=20) are on screen...
    for i in 1..=8 {
        assert!(
            text.contains(&format!("msg line {i:02}")),
            "head row {i}:\n{text}"
        );
    }
    assert!(text.contains("msg line 19"), "tail row:\n{text}");
    assert!(text.contains("msg line 20"), "tail row:\n{text}");
    // ...the hidden middle (9..=18, 10 rows) is replaced by one marker.
    for i in 9..=18 {
        assert!(
            !text.contains(&format!("msg line {i:02}")),
            "hidden row {i} leaked:\n{text}"
        );
    }
    assert!(
        text.contains("... +10 lines hidden (Ctrl+O expand)"),
        "fold marker missing:\n{text}"
    );
}

#[test]
fn ctrl_o_unfolds_the_message() {
    let mut app = App::new();
    app.push_user(&long_message(20));
    // The same switch that expands thinking unfolds long input.
    app.show_thoughts = true;
    let text = snapshot_text(&mut app, 80, 40);
    for i in 1..=20 {
        assert!(
            text.contains(&format!("msg line {i:02}")),
            "row {i}:\n{text}"
        );
    }
    assert!(
        !text.contains("lines hidden"),
        "marker must vanish:\n{text}"
    );
}

#[test]
fn short_messages_never_fold() {
    let mut app = App::new();
    // At the threshold (12 rows) the marker would not save space — no fold.
    app.push_user(&long_message(12));
    let text = snapshot_text(&mut app, 80, 40);
    for i in 1..=12 {
        assert!(
            text.contains(&format!("msg line {i:02}")),
            "row {i}:\n{text}"
        );
    }
    assert!(
        !text.contains("lines hidden"),
        "no fold at threshold:\n{text}"
    );
}

#[test]
fn consecutive_messages_fold_independently() {
    let mut app = App::new();
    app.push_user(&long_message(20));
    app.push_user("short question");
    let text = snapshot_text(&mut app, 80, 40);
    // The long message folds on its own — the short one right behind it
    // shows whole, not swallowed into the first fold's tail.
    assert!(
        text.contains("... +10 lines hidden (Ctrl+O expand)"),
        "first message folds:\n{text}"
    );
    assert!(
        text.contains("short question"),
        "second message whole:\n{text}"
    );
    assert!(text.contains("msg line 20"), "first message tail:\n{text}");
    assert!(
        !text.contains("msg line 09"),
        "first message middle:\n{text}"
    );
}
