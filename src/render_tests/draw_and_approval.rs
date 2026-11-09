//! Draw smoke tests and the approval popup, including its product copy.

use super::super::*;
use super::common::*;
use phi_tui::lines::OutputLine;

#[test]
fn draw_does_not_panic_on_populated_state() {
    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = populated_app();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
}

#[test]
fn draw_empty_and_scrolled_states() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    // Fresh (empty) state.
    let mut app = App::new();
    terminal.draw(|f| draw(f, &mut app)).unwrap();

    // Scrolled up (not following bottom).
    let mut app = App::new();
    for i in 0..100 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            detail: None,
            text: format!("long line {i}"),
            kind: LineKind::Normal,
            tool_state: None,
        });
    }
    app.viewport.follow_bottom = false;
    app.viewport.scroll_offset = 50;
    terminal.draw(|f| draw(f, &mut app)).unwrap();
}

#[test]
fn draw_cursor_mid_multibyte_line() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    app.composer.insert_str("héllo");
    app.composer.move_home();
    app.composer.move_right(); // cursor after 'h' (byte 1), before the 2-byte 'é'
    terminal.draw(|f| draw(f, &mut app)).unwrap();
}

#[test]
fn draw_with_pending_approval_popup() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    app.approval_queue.push_back(crate::approval::ApprovalItem {
        request: phi_agent::ApprovalRequest {
            title: "write_file".into(),
            message: "Write file: src/lib.rs".into(),
            action_key: None,
            risk_level: phi_agent::RiskLevel::Destructive,
            raw: None,
            source: None,
        },
        decision_tx: tx,
    });
    terminal.draw(|f| draw(f, &mut app)).unwrap();
}

#[test]
fn snapshot_text_captures_layout_and_popup() {
    let mut app = App::new();
    app.push_system("hello");
    app.composer.insert_str("do a thing");
    let (tx, _rx) = tokio::sync::oneshot::channel();
    app.approval_queue.push_back(crate::approval::ApprovalItem {
        request: phi_agent::ApprovalRequest {
            title: "write_file".into(),
            message: "Write file: src/lib.rs".into(),
            action_key: None,
            risk_level: phi_agent::RiskLevel::Sensitive,
            raw: None,
            source: None,
        },
        decision_tx: tx,
    });

    let text = snapshot_text(&mut app, 80, 24);
    assert!(text.contains("approval"), "popup title missing:\n{text}");
    assert!(
        text.contains("do a thing"),
        "composer content missing:\n{text}"
    );
    assert!(
        text.contains('\n'),
        "snapshot should be multi-line:\n{text}"
    );
}

#[test]
fn approval_popup_shows_sub_agent_source() {
    let mut app = App::new();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    app.approval_queue.push_back(crate::approval::ApprovalItem {
        request: phi_agent::ApprovalRequest {
            title: "write_file".into(),
            message: "Write file: src/lib.rs".into(),
            action_key: None,
            risk_level: phi_agent::RiskLevel::Sensitive,
            raw: None,
            source: Some("root/coder-1".into()),
        },
        decision_tx: tx,
    });
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        text.contains("root/coder-1"),
        "popup must name the requesting sub-agent:\n{text}"
    );
    assert!(
        text.contains("sub-agent"),
        "popup must label the source as a sub-agent:\n{text}"
    );
}

#[test]
fn approval_popup_without_source_renders_unchanged() {
    let mut app = App::new();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    app.approval_queue.push_back(crate::approval::ApprovalItem {
        request: phi_agent::ApprovalRequest {
            title: "write_file".into(),
            message: "Write file: src/lib.rs".into(),
            action_key: None,
            risk_level: phi_agent::RiskLevel::Sensitive,
            raw: None,
            source: None,
        },
        decision_tx: tx,
    });
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        !text.contains("sub-agent"),
        "no source → no sub-agent label (the main agent's rendering is unchanged):\n{text}"
    );
    assert!(text.contains("Write file: src/lib.rs"));
}

fn push_approval_with_message(app: &mut App, message: String) {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    app.approval_queue.push_back(crate::approval::ApprovalItem {
        request: phi_agent::ApprovalRequest {
            title: "write_file".into(),
            message,
            action_key: None,
            risk_level: phi_agent::RiskLevel::Destructive,
            raw: None,
            source: None,
        },
        decision_tx: tx,
    });
}

#[test]
fn approval_popup_footer_survives_a_long_message() {
    // BUG 1: the y/a/n legend used to be the tail of one wrapped paragraph —
    // a long message pushed it out of the fixed 70%x50% box and the user got
    // an approval prompt with no visible keys. The legend is now pinned.
    let mut app = App::new();
    let long = (0..60)
        .map(|i| format!("step {i}: do the thing"))
        .collect::<Vec<_>>()
        .join("\n");
    push_approval_with_message(&mut app, long);
    let text = snapshot_text(&mut app, 80, 24);
    for legend in ["[y] allow once", "[a] allow always", "[n] deny"] {
        assert!(text.contains(legend), "legend {legend:?} clipped:\n{text}");
    }
}

#[test]
fn approval_popup_footer_survives_a_narrow_terminal() {
    // Narrower than the legend itself: it wraps onto its reserved footer rows
    // rather than being pushed off-screen (every decision stays visible).
    let mut app = App::new();
    push_approval_with_message(&mut app, "Write file: src/lib.rs".into());
    let text = snapshot_text(&mut app, 40, 12);
    for legend in ["allow once", "allow always", "deny"] {
        assert!(
            text.contains(legend),
            "legend {legend:?} clipped at 40x12:\n{text}"
        );
    }
}

#[test]
fn approval_popup_sizes_to_a_small_request() {
    // Content-sized box: a one-line request no longer gets the giant 70%x50%
    // frame (the legend and title still render).
    let mut app = App::new();
    push_approval_with_message(&mut app, "Write file: src/lib.rs".into());
    let text = snapshot_text(&mut app, 100, 40);
    assert!(text.contains("[y] allow once"), "legend missing:\n{text}");
    assert!(text.contains("write_file"), "title missing:\n{text}");
}

#[test]
fn approval_footer_spans_concatenate_to_the_budgeted_legend() {
    // The popup's width/height budget is measured from the prompt's legend; the
    // styled footer line must concatenate to exactly that text or the box
    // under-measures what it renders (or clips the legend).
    let mut app = App::new();
    push_approval_with_message(&mut app, "Write file: src/lib.rs".into());
    for sel in [
        None,
        Some(ApprovalDecision::AllowOnce),
        Some(ApprovalDecision::AllowAlways),
        Some(ApprovalDecision::Deny),
    ] {
        app.approval_selection = sel;
        let line = approval_footer_line(&app, false);
        let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, footer_legend(&app.copy.approval, false));
    }
}

#[test]
fn consent_footer_spans_concatenate_to_the_budgeted_legend() {
    // Canvas consent (1C) wears the two-key legend — same honesty contract.
    let mut app = App::new();
    push_approval_with_message(&mut app, "Open the design canvas?".into());
    for sel in [
        None,
        Some(ApprovalDecision::AllowOnce),
        Some(ApprovalDecision::Deny),
    ] {
        app.approval_selection = sel;
        let line = approval_footer_line(&app, true);
        let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, footer_legend(&app.copy.approval, true));
        assert!(joined.contains("[y] open browser"), "legend: {joined}");
        assert!(joined.contains("[n] skip"), "legend: {joined}");
        assert!(!joined.contains("[a]"), "no standing consent key: {joined}");
    }
}

#[test]
fn approval_footer_highlights_the_selected_option() {
    // BUG 4 (final form): letters only select — the highlight is the only
    // signal that one is picked; Enter is inert (dimmed) until then.
    let mut app = App::new();
    push_approval_with_message(&mut app, "Write file: src/lib.rs".into());
    app.approval_selection = Some(ApprovalDecision::AllowAlways);
    let line = approval_footer_line(&app, false);
    let bold: Vec<&str> = line
        .spans
        .iter()
        .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(bold, vec!["[a] allow always", "[Enter] confirm"]);
    app.approval_selection = None;
    let line = approval_footer_line(&app, false);
    assert!(
        line.spans
            .iter()
            .all(|s| !s.style.add_modifier.contains(Modifier::BOLD)),
        "nothing selected → nothing highlighted:\n{line:?}"
    );
}

#[test]
fn consent_footer_highlights_the_selected_option() {
    // Same select-then-Enter contract on the consent gate.
    let mut app = App::new();
    push_approval_with_message(&mut app, "Open the design canvas?".into());
    app.approval_selection = Some(ApprovalDecision::AllowOnce);
    let line = approval_footer_line(&app, true);
    let bold: Vec<&str> = line
        .spans
        .iter()
        .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(bold, vec!["[y] open browser", "[Enter] confirm"]);
}

#[test]
fn approval_popup_wears_the_products_copy() {
    // T14: the popup's words are product copy — an override must reach the
    // head line, the option labels and the width-budgeted legend alike.
    let mut app = App::new();
    app.set_copy(crate::shell::CopyTable {
        approval: crate::shell::ApprovalCopy {
            title: "!! Custom approval".to_string(),
            allow_once: "[y] once only".to_string(),
            allow_always: "[a] always".to_string(),
            deny: "[n] never".to_string(),
            confirm: "[Enter] go".to_string(),
            ..crate::shell::ApprovalCopy::default()
        },
        ..crate::shell::CopyTable::default()
    });
    push_approval_with_message(&mut app, "Write file: src/lib.rs".into());
    let text = snapshot_text(&mut app, 100, 40);
    assert!(text.contains("!! Custom approval"), "head line:\n{text}");
    assert!(text.contains("[y] once only"), "legend:\n{text}");
    // The honesty contract holds under custom copy too.
    let line = approval_footer_line(&app, false);
    let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(joined, footer_legend(&app.copy.approval, false));
    assert_eq!(
        joined,
        "[y] once only   [a] always   [n] never   -   [Enter] go"
    );
}

#[test]
fn risk_line_is_product_copy_the_severity_color_is_not() {
    // T14: the words come from the product formatter; the color stays shell
    // mechanism keyed on the level.
    let mut app = App::new();
    app.set_copy(crate::shell::CopyTable {
        approval: crate::shell::ApprovalCopy {
            risk_line: std::sync::Arc::new(|level| {
                if matches!(level, phi_agent::RiskLevel::Destructive) {
                    "Careful: breaks things".to_string()
                } else {
                    "Risk: fine".to_string()
                }
            }),
            ..crate::shell::ApprovalCopy::default()
        },
        ..crate::shell::CopyTable::default()
    });
    push_approval_with_message(&mut app, "Write file: src/lib.rs".into());
    let text = snapshot_text(&mut app, 100, 40);
    assert!(
        text.contains("Careful: breaks things"),
        "risk line:\n{text}"
    );
    assert!(
        !text.contains("Risk: Destructive"),
        "default leaked:\n{text}"
    );
}

#[test]
fn mode_words_are_product_copy() {
    // T14: badge + switch notice are product wording (the default notice is
    // what `app_tests` pins via `cycle_approval_mode`).
    let mut app = App::new();
    app.set_copy(crate::shell::CopyTable {
        approval: crate::shell::ApprovalCopy {
            mode_badge: std::sync::Arc::new(|_| "custom badge".to_string()),
            mode_notice: std::sync::Arc::new(|_| "custom notice".to_string()),
            ..crate::shell::ApprovalCopy::default()
        },
        ..crate::shell::CopyTable::default()
    });
    app.cycle_approval_mode();
    assert!(
        app.status_line().contains("custom notice"),
        "notice: {}",
        app.status_line()
    );
    let text = snapshot_text(&mut app, 80, 24);
    assert!(text.contains("custom badge"), "badge:\n{text}");
}
