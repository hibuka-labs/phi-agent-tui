//! Mention and slash completion, including the twin key-sequence snapshots.

use super::super::*;

fn mention_scratch(tag: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("phimint-app-mention-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn at_opens_mention_picker_listing_root() {
    let mut app = App::new();
    let root = mention_scratch("open");
    std::fs::write(root.join("a.txt"), "x").unwrap();
    app.set_workspace_root(root.clone());

    assert_eq!(app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE), None);
    let m = app.mention().expect("mention open");
    assert_eq!(m.prefix(), "");
    assert!(m.entries()[0].synthetic, "synthetic row is first");
    assert!(m.entries().iter().any(|e| e.name == "a.txt"));
    assert_eq!(app.composer.text(), "@");
}

#[test]
fn enter_on_empty_prefix_inserts_dot_and_closes() {
    let mut app = App::new();
    let root = mention_scratch("enter-empty");
    app.set_workspace_root(root.clone());

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.mention().is_none());
    assert_eq!(app.composer.text(), ".");
}

#[test]
fn dotdot_prefix_inserts_absolute_parent_path() {
    let mut app = App::new();
    let root = mention_scratch("dotdot-app");
    app.set_workspace_root(root.clone());

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    for c in "../".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert!(app.mention().is_some(), "picker stays open while typing");
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let text = app.composer.text();
    assert!(!text.contains('@'), "composer holds the path, got {text:?}");
    assert!(
        std::path::Path::new(&text).is_absolute(),
        "outside path is absolute, got {text:?}"
    );
}

#[test]
fn typing_name_then_arrow_selects_file() {
    let mut app = App::new();
    let root = mention_scratch("select");
    std::fs::write(root.join("main.rs"), "x").unwrap();
    std::fs::write(root.join("other.rs"), "x").unwrap();
    app.set_workspace_root(root.clone());

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    for c in "main".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    // Arrow down off the synthetic row onto the single real match, accept it.
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.composer.text(), "main.rs");
}

#[test]
fn esc_with_typed_prefix_keeps_text_and_closes() {
    let mut app = App::new();
    let root = mention_scratch("esc-keep");
    app.set_workspace_root(root.clone());

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    for c in "src".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.mention().is_none());
    assert_eq!(
        app.composer.text(),
        "@src",
        "Esc closes the picker but keeps the typed text"
    );
}

#[test]
fn esc_after_dir_navigation_keeps_text() {
    let mut app = App::new();
    let root = mention_scratch("esc-dir");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/a.rs"), "x").unwrap();
    app.set_workspace_root(root.clone());

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE); // descend into sub/
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE); // close the picker
    assert!(app.mention().is_none());
    assert_eq!(
        app.composer.text(),
        "@sub/",
        "Esc keeps the typed @path in the composer"
    );
}

#[test]
fn esc_on_bare_at_removes_trigger() {
    let mut app = App::new();
    let root = mention_scratch("esc-bare");
    app.set_workspace_root(root.clone());

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.mention().is_none());
    assert_eq!(app.composer.text(), "");
}

#[test]
fn enter_on_directory_focuses_the_directory_row() {
    let mut app = App::new();
    let root = mention_scratch("dir-focus");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/a.rs"), "x").unwrap();
    std::fs::write(root.join("top.txt"), "x").unwrap();
    app.set_workspace_root(root.clone());

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    // Arrow onto `sub` — the first real entry, right after the synthetic row.
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let m = app.mention().expect("picker stays open on a directory");
    assert_eq!(m.prefix(), "sub/");
    assert_eq!(
        m.selected_index(),
        0,
        "focus is on the entered directory (synthetic row)"
    );
    assert!(m.entries()[0].synthetic);
    assert_eq!(app.composer.text(), "@sub/");
}

#[test]
fn backspace_with_empty_prefix_cancels_mention() {
    let mut app = App::new();
    let root = mention_scratch("bsp");
    app.set_workspace_root(root.clone());

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert!(app.mention().is_none());
    assert_eq!(app.composer.text(), "");
}

#[test]
fn mention_inserts_after_existing_text() {
    let mut app = App::new();
    let root = mention_scratch("mid");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    app.set_workspace_root(root.clone());

    app.composer.insert_str("read ");
    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    for c in "sub".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.composer.text(), "read sub");
}

#[test]
fn paste_routes_to_mention_prefix_when_open() {
    let mut app = App::new();
    let root = mention_scratch("paste");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    app.set_workspace_root(root.clone());

    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    app.paste("sub");
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.composer.text(), "sub");
}

// ── / skill picker tests ──

fn app_with_skills() -> App {
    let mut app = App::new();
    // After sorting: commit(0), requesting-code-review(1), review(2)
    app.set_skill_summaries(vec![
        ("commit".into(), "Generate a commit message".into()),
        (
            "requesting-code-review".into(),
            "Request a code review".into(),
        ),
        ("review".into(), "Pre-landing PR review".into()),
    ]);
    app
}

#[test]
fn slash_opens_picker_on_empty_composer() {
    let mut app = app_with_skills();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(app.slash().is_some(), "slash picker should open");
    assert_eq!(app.composer.text(), "/");
}

#[test]
fn slash_does_not_open_when_composer_not_empty() {
    let mut app = app_with_skills();
    app.handle_key(KeyCode::Char('h'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(
        app.slash().is_none(),
        "slash picker should not open mid-input"
    );
}

#[test]
fn slash_filters_by_prefix() {
    let mut app = app_with_skills();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);

    let s = app.slash().unwrap();
    let names: Vec<&str> = s.entries().iter().map(|(n, _)| n.as_str()).collect();
    // "commit" starts with 'c' → kept
    assert!(names.contains(&"commit"), "commit should match prefix 'c'");
    // "requesting-code-review" has 'c' but does NOT start with 'c' → filtered out
    assert!(
        !names.contains(&"requesting-code-review"),
        "requesting-code-review should not match prefix 'c'"
    );
    // "review" does not start with 'c' → filtered out
    assert!(
        !names.contains(&"review"),
        "review should not match prefix 'c'"
    );
    // Only "commit" remains
    assert_eq!(names.len(), 1, "only 'commit' should match prefix 'c'");
}

#[test]
fn slash_filters_progressively() {
    let mut app = app_with_skills();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    // All 3 visible initially
    assert_eq!(app.slash().unwrap().entries().len(), 3);

    // Type 'r' → "requesting-code-review" and "review" start with 'r'
    app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
    let names: Vec<&str> = app
        .slash()
        .unwrap()
        .entries()
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names.len(), 2, "'r' should match 2 skills");
    assert!(names.contains(&"requesting-code-review"));
    assert!(names.contains(&"review"));

    // Type 'e' → "review" starts with "re", "requesting-code-review" starts with "re"
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    let names: Vec<&str> = app
        .slash()
        .unwrap()
        .entries()
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names.len(), 2, "'re' should still match 2 skills");

    // Type 'v' → only "review" starts with "rev"
    app.handle_key(KeyCode::Char('v'), KeyModifiers::NONE);
    let names: Vec<&str> = app
        .slash()
        .unwrap()
        .entries()
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names.len(), 1, "'rev' should match only 'review'");
    assert!(names.contains(&"review"));

    // Composer should show "/rev"
    assert_eq!(app.composer.text(), "/rev");
}

#[test]
fn slash_backspace_widens_filter() {
    let mut app = app_with_skills();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('v'), KeyModifiers::NONE);
    assert_eq!(app.slash().unwrap().entries().len(), 1); // only "review"

    // Backspace → "re" → both "review" and "requesting-code-review" match again
    app.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    let names: Vec<&str> = app
        .slash()
        .unwrap()
        .entries()
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names.len(), 2, "backspace to 're' should widen filter");
    assert_eq!(app.composer.text(), "/re");
}

#[test]
fn slash_enter_confirms_selection() {
    let mut app = app_with_skills();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    // The first entry is selected by default ("commit" after sorting)
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.slash().is_none(), "picker should close after Enter");
    assert_eq!(
        app.composer.text(),
        "/commit ",
        "should insert /name + space"
    );
}

#[test]
fn slash_esc_cancels() {
    let mut app = app_with_skills();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.slash().is_none(), "picker should close on Esc");
    assert!(
        app.composer.is_empty(),
        "composer should be empty after cancel"
    );
}

/// Zero matches + Enter = the real command is still sent (`/tools off <key>`),
/// not swallowed whole. Regression: in session 20261015_35ab9031
/// `/tools off add_tool` was erased every time by the picker's Cancelled
/// semantics, so the command never reached parse_command.
#[test]
fn slash_enter_with_no_match_submits_the_typed_command() {
    let mut app = app_with_skills();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    for c in "tools off add_tool".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    // Zero matches (the fixture has no tools skill) — the picker is still open.
    assert_eq!(app.slash().unwrap().entries_count(), 0);
    let action = app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.slash().is_none(), "picker should close after Enter");
    assert_eq!(
        action,
        Some(Action::Submit("/tools off add_tool".into())),
        "no-match Enter must submit the typed text verbatim"
    );
    assert!(app.composer.is_empty(), "composer cleared on submit");
}

/// Zero matches + Enter, but the agent is running: no submit (same criterion as
/// a plain Enter), just close the picker.
#[test]
fn slash_enter_with_no_match_while_running_closes_picker_without_submit() {
    let mut app = app_with_skills();
    app.running = true;
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    for c in "tools off add_tool".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    let action = app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.slash().is_none(), "picker should close after Enter");
    assert_eq!(action, None, "no submit while running");
    assert_eq!(
        app.composer.text(),
        "/tools off add_tool",
        "text stays in the composer for later"
    );
}

#[test]
fn slash_arrow_keys_navigate() {
    let mut app = app_with_skills();
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);

    assert_eq!(app.slash().unwrap().selected_index(), 0);

    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.slash().unwrap().selected_index(), 1);

    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.slash().unwrap().selected_index(), 2);

    // Already at the end: Down does nothing
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.slash().unwrap().selected_index(), 2);

    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.slash().unwrap().selected_index(), 1);
}

// ── Twin key-sequence snapshot characterization ──
//
// Merge guard for the mention/slash key handlers. Both handlers run one
// skeleton — capture the old prefix, branch on `CompleterAction`, account the
// removal as `1 + old_prefix.chars().count()`, then sync the composer to the
// prefix delta on `Continue` — with three policy one-liners between them
// (mention keeps a typed prefix on Esc, slash turns no-match Enter into a
// submit unless the agent is running, mention mirrors a stray char into the
// composer). Each step below records what that skeleton leaves observable:
// composer text, the open picker with its prefix, and the returned action.
// A mechanical merge that breaks any stage fails on the exact key that
// diverged. Characterization only — these rows pin current behavior on
// purpose; incident redlines stay in their own named tests above.

/// Short, stable key label for snapshot rows.
fn key_label(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "<Space>".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "<Enter>".into(),
        KeyCode::Esc => "<Esc>".into(),
        KeyCode::Backspace => "<Bksp>".into(),
        KeyCode::Down => "<Down>".into(),
        KeyCode::Up => "<Up>".into(),
        other => format!("{other:?}"),
    }
}

/// One snapshot row: key, composer text, open picker + prefix, returned action.
fn snap(app: &App, code: KeyCode, action: &Option<Action>) -> String {
    let picker = match (app.mention(), app.slash()) {
        (Some(m), _) => format!("mention({:?})", m.prefix()),
        (None, Some(s)) => format!("slash({:?})", s.prefix()),
        (None, None) => "closed".to_string(),
    };
    format!(
        "{} => text={:?} {} act={:?}",
        key_label(code),
        app.composer.text(),
        picker,
        action
    )
}

/// Play `keys` through the real key entry point, one snapshot per step.
fn key_trace(app: &mut App, keys: &[KeyCode]) -> Vec<String> {
    keys.iter()
        .map(|&code| {
            let action = app.handle_key(code, KeyModifiers::NONE);
            snap(app, code, &action)
        })
        .collect()
}

#[test]
fn twin_snapshot_mention_typing_and_esc_keeps_typed_text() {
    let mut app = App::new();
    let root = mention_scratch("twin-esc");
    std::fs::write(root.join("main.rs"), "x").unwrap();
    app.set_workspace_root(root.clone());

    let trace = key_trace(
        &mut app,
        &[
            KeyCode::Char('@'),
            KeyCode::Char('m'),
            KeyCode::Char('a'),
            KeyCode::Char('i'),
            KeyCode::Char('n'),
            KeyCode::Esc,
        ],
    );
    assert_eq!(
        trace,
        vec![
            r#"@ => text="@" mention("") act=None"#,
            r#"m => text="@m" mention("m") act=None"#,
            r#"a => text="@ma" mention("ma") act=None"#,
            r#"i => text="@mai" mention("mai") act=None"#,
            r#"n => text="@main" mention("main") act=None"#,
            r#"<Esc> => text="@main" closed act=None"#,
        ]
    );
}

/// Multi-byte prefix: `chars().count()` accounting (not bytes) on both the
/// Continue prefix-delta sync and the backspace-out cancel.
#[test]
fn twin_snapshot_mention_multibyte_prefix_backspace_accounting() {
    let mut app = App::new();
    let root = mention_scratch("twin-multibyte-bs");
    std::fs::write(root.join("中文.txt"), "x").unwrap();
    app.set_workspace_root(root.clone());

    let trace = key_trace(
        &mut app,
        &[
            KeyCode::Char('@'),
            KeyCode::Char('中'),
            KeyCode::Char('文'),
            KeyCode::Backspace,
            KeyCode::Backspace,
            KeyCode::Backspace,
        ],
    );
    assert_eq!(
        trace,
        vec![
            r#"@ => text="@" mention("") act=None"#,
            "中 => text=\"@中\" mention(\"中\") act=None",
            "文 => text=\"@中文\" mention(\"中文\") act=None",
            "<Bksp> => text=\"@中\" mention(\"中\") act=None",
            r#"<Bksp> => text="@" mention("") act=None"#,
            r#"<Bksp> => text="" closed act=None"#,
        ]
    );
}

/// Multi-byte prefix on the Selected branch: the `@prefix` removal is
/// `1 + old_prefix.chars().count()` chars, then the path lands verbatim.
#[test]
fn twin_snapshot_mention_multibyte_select_replaces_trigger() {
    let mut app = App::new();
    let root = mention_scratch("twin-multibyte-sel");
    std::fs::write(root.join("中文.txt"), "x").unwrap();
    app.set_workspace_root(root.clone());

    let trace = key_trace(
        &mut app,
        &[
            KeyCode::Char('@'),
            KeyCode::Char('中'),
            KeyCode::Down,
            KeyCode::Enter,
        ],
    );
    assert_eq!(
        trace,
        vec![
            r#"@ => text="@" mention("") act=None"#,
            "中 => text=\"@中\" mention(\"中\") act=None",
            "<Down> => text=\"@中\" mention(\"中\") act=None",
            "<Enter> => text=\"中文.txt\" closed act=None",
        ]
    );
}

#[test]
fn twin_snapshot_slash_filter_and_backspace_widen() {
    let mut app = app_with_skills();
    let trace = key_trace(
        &mut app,
        &[
            KeyCode::Char('/'),
            KeyCode::Char('r'),
            KeyCode::Char('e'),
            KeyCode::Char('v'),
            KeyCode::Backspace,
        ],
    );
    assert_eq!(
        trace,
        vec![
            r#"/ => text="/" slash("") act=None"#,
            r#"r => text="/r" slash("r") act=None"#,
            r#"e => text="/re" slash("re") act=None"#,
            r#"v => text="/rev" slash("rev") act=None"#,
            r#"<Bksp> => text="/re" slash("re") act=None"#,
        ]
    );
}

/// Selected with a typed prefix: removal is `1 + old_prefix.chars().count()`
/// chars, insertion is `/{name} `.
#[test]
fn twin_snapshot_slash_select_replaces_prefix() {
    let mut app = app_with_skills();
    let trace = key_trace(
        &mut app,
        &[
            KeyCode::Char('/'),
            KeyCode::Char('r'),
            KeyCode::Down,
            KeyCode::Enter,
        ],
    );
    assert_eq!(
        trace,
        vec![
            r#"/ => text="/" slash("") act=None"#,
            r#"r => text="/r" slash("r") act=None"#,
            r#"<Down> => text="/r" slash("r") act=None"#,
            r#"<Enter> => text="/review " closed act=None"#,
        ]
    );
}

/// Empty-match Enter while idle submits the typed command verbatim
/// (the 20261015_35ab9031 incident, spelled out key by key).
#[test]
fn twin_snapshot_slash_no_match_enter_idle_submits() {
    let mut app = app_with_skills();
    let mut keys = vec![KeyCode::Char('/')];
    keys.extend("tools off add_tool".chars().map(KeyCode::Char));
    keys.push(KeyCode::Enter);

    let trace = key_trace(&mut app, &keys);
    assert_eq!(
        trace.last().map(String::as_str),
        Some(r#"<Enter> => text="" closed act=Some(Submit("/tools off add_tool"))"#),
        "full trace: {trace:#?}"
    );
    // Every intermediate step just echoes the typed prefix into the composer.
    assert_eq!(trace[0], r#"/ => text="/" slash("") act=None"#);
    for (i, c) in "tools off add_tool".chars().enumerate() {
        let typed: String = "tools off add_tool".chars().take(i + 1).collect();
        let expected = format!(
            "{} => text={:?} slash({:?}) act=None",
            key_label(KeyCode::Char(c)),
            format!("/{typed}"),
            typed
        );
        assert_eq!(trace[i + 1], expected, "step {}", i + 1);
    }
}

/// Empty-match Enter while the agent runs: close the picker, no submit, the
/// typed command stays in the composer (same criterion as a plain Enter).
#[test]
fn twin_snapshot_slash_no_match_enter_while_running_keeps_text() {
    let mut app = app_with_skills();
    app.running = true;
    let mut keys = vec![KeyCode::Char('/')];
    keys.extend("tools off add_tool".chars().map(KeyCode::Char));
    keys.push(KeyCode::Enter);

    let trace = key_trace(&mut app, &keys);
    assert_eq!(
        trace.last().map(String::as_str),
        Some(r#"<Enter> => text="/tools off add_tool" closed act=None"#),
        "full trace: {trace:#?}"
    );
}

/// Multi-byte prefix on the slash side: Esc wipes `1 + old_prefix.chars()`
/// chars (no keep-text policy here — the twin delta against mention, pinned
/// against the mirror sequence in `twin_snapshot_mention_typing_and_esc_*`).
#[test]
fn twin_snapshot_slash_multibyte_cancel_wipes_trigger_and_prefix() {
    let mut app = app_with_skills();
    let trace = key_trace(
        &mut app,
        &[KeyCode::Char('/'), KeyCode::Char('中'), KeyCode::Esc],
    );
    assert_eq!(
        trace,
        vec![
            r#"/ => text="/" slash("") act=None"#,
            "中 => text=\"/中\" slash(\"中\") act=None",
            r#"<Esc> => text="" closed act=None"#,
        ]
    );
}

/// Multi-byte prefix on the slash Selected branch: `/{name} ` insertion after
/// a `chars()`-counted removal.
#[test]
fn twin_snapshot_slash_multibyte_select_replaces_prefix() {
    let mut app = App::new();
    app.set_skill_summaries(vec![
        ("commit".into(), "Generate a commit message".into()),
        ("中文".into(), "中文说明".into()),
    ]);
    let trace = key_trace(
        &mut app,
        &[KeyCode::Char('/'), KeyCode::Char('中'), KeyCode::Enter],
    );
    assert_eq!(
        trace,
        vec![
            r#"/ => text="/" slash("") act=None"#,
            "中 => text=\"/中\" slash(\"中\") act=None",
            "<Enter> => text=\"/中文 \" closed act=None",
        ]
    );
}
