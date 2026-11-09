//! Snapshot coverage: status bar, sub-agent strip, task panel, popups, banner, and diff blocks.

use super::super::*;
use crate::app::{BackgroundTaskEntry, SubAgentState, SubAgentStatus, TuiEvent};
use crate::theme::BrandRow;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use phi_agent::{RuntimeEvent, SessionId};
use phi_kernel_tools::background_shell::BackgroundTaskStatus;
use phi_tui::lines::OutputLine;

/// Locate `needle`'s first cell and return its style. The row is walked the way
/// `buffer_to_text` does it (step over a wide glyph's continuation cell) and
/// the needle's byte offset maps back to a screen column, since CJK cells are
/// multi-byte — the same trick as the banner test.
fn cell_style_for(buf: &Buffer, width: usize, needle: &str) -> Option<Style> {
    let area = buf.area;
    for y in 0..area.height as usize {
        let mut row = String::new();
        let mut col_of_byte: Vec<usize> = Vec::new();
        let mut x = 0usize;
        while x < width {
            let sym = buf.content()[y * width + x].symbol();
            for _ in 0..sym.len() {
                col_of_byte.push(x);
            }
            row.push_str(sym);
            x += sym.width().max(1);
        }
        if let Some(byte) = row.find(needle) {
            return Some(buf.content()[y * width + col_of_byte[byte]].style());
        }
    }
    None
}

#[test]
fn status_bar_shows_the_canvas_state_words() {
    // Row 13: the state words are spec-locked and ride the left slot, with the
    // mode block keeping its columns beside them.
    let mut app = App::new();
    app.panel_status = Some(crate::shell::StatusFace {
        text: "canvas ● localhost:4242 · 3 options to pick".into(),
        alert: false,
    });
    let text = snapshot_text(&mut app, 100, 12);
    assert!(
        text.contains("canvas ● localhost:4242 · 3 options to pick"),
        "canvas face missing from the status bar:\n{text}"
    );
    assert!(
        text.contains("(shift+tab to cycle)"),
        "the mode block keeps its columns:\n{text}"
    );

    // The live face is calm green, not the crash face's shout.
    let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let style = cell_style_for(&buf, 100, "canvas ● localhost").expect("canvas face rendered");
    assert_eq!(style.fg, Some(app.theme().good));
    assert!(!style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn status_bar_crash_face_is_red_and_bold() {
    // Row 16: the disconnected face is the one that shouts.
    let mut app = App::new();
    app.panel_status = Some(crate::shell::StatusFace {
        text: "canvas ● disconnected (ctrl+b reopen)".into(),
        alert: true,
    });
    let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let style = cell_style_for(&buf, 100, "canvas ● disconnected").expect("crash face rendered");
    assert_eq!(style.fg, Some(Color::Red));
    assert!(style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn window_range_shifts_by_scroll_offset() {
    let mut app = App::new();
    for i in 0..100 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            text: format!("line {i}"),
            kind: LineKind::Normal,
            detail: None,
            tool_state: None,
        });
    }
    assert_eq!(app.viewport.window_range(100, 30), 70..100);
    app.viewport.follow_bottom = false;
    app.viewport.scroll_offset = 10;
    assert_eq!(app.viewport.window_range(100, 30), 60..90);
    // Past the top clamps to the first `height` lines.
    app.viewport.scroll_offset = 1000;
    assert_eq!(app.viewport.window_range(100, 30), 0..30);
}

#[test]
fn streaming_tail_renders_in_snapshot() {
    let mut app = App::new();
    app.running = true;
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::TextDelta {
        session_id: SessionId::new(1),
        text: "partial answer".to_string(),
        agent_id: None,
        trace_id: None,
    }));
    let text = snapshot_text(&mut app, 80, 24);
    assert!(text.contains("partial answer"), "tail missing:\n{text}");
}

#[test]
fn buffer_to_text_skips_wide_char_continuation() {
    let backend = TestBackend::new(12, 2);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            f.render_widget(Paragraph::new("你好"), f.area());
        })
        .unwrap();
    let text = buffer_to_text(terminal.backend().buffer());
    assert!(text.contains("你好"), "got: {text:?}");
    assert!(
        !text.contains("你 好"),
        "wide chars should be adjacent, got: {text:?}"
    );
}

#[test]
fn snapshot_shows_sub_agent_strip() {
    let mut app = App::new();
    app.sub_agents.insert(
        "root/a".to_string(),
        SubAgentState {
            name: "a".to_string(),
            status: SubAgentStatus::Running,
            files: Vec::new(),
            started_at: std::time::Instant::now(),
            completed_at: None,
            last_tool_at: std::time::Instant::now(),
            events: Vec::new(),
        },
    );
    app.sub_agents.insert(
        "root/b".to_string(),
        SubAgentState {
            name: "b".to_string(),
            status: SubAgentStatus::Done,
            files: Vec::new(),
            started_at: std::time::Instant::now(),
            completed_at: Some(std::time::Instant::now()),
            last_tool_at: std::time::Instant::now(),
            events: Vec::new(),
        },
    );
    // The frame is opt-in (default S0 collapsed) — open it to see the rows.
    // Row b is touched: an untouched done row hides the moment it lands
    // (hide-on-complete 完成即隐), a touched one lingers as a dim corpse until a sweep.
    app.task_panel.touched.insert("root/b".to_string());
    app.task_panel.frame_open = true;
    let text = snapshot_text(&mut app, 80, 24);
    assert!(text.contains("* a"), "running marker missing:\n{text}");
    assert!(text.contains("+ b"), "done marker missing:\n{text}");
}

#[test]
fn snapshot_omits_strip_when_no_sub_agents() {
    let mut app = App::new();
    let text = snapshot_text(&mut app, 80, 24);
    assert!(!text.contains("* ["), "strip should be absent:\n{text}");
}

#[test]
fn task_panel_lists_background_tasks_as_rows() {
    // Panel policy (2026-10-29): background shell tasks are rows next to
    // sub-agents — command summary, status, deadline — and selecting one
    // opens its live output tail. They still live as transcript tool-call
    // records + the status-bar counter; the panel is the inspection surface.
    let mut app = App::new();
    app.sub_agents.insert(
        "root/a".to_string(),
        SubAgentState {
            name: "a".to_string(),
            status: SubAgentStatus::Running,
            files: Vec::new(),
            started_at: std::time::Instant::now(),
            completed_at: None,
            last_tool_at: std::time::Instant::now(),
            events: Vec::new(),
        },
    );
    app.background_tasks.insert(
        "bg_aaaa1111".to_string(),
        BackgroundTaskEntry {
            id: "bg_aaaa1111".to_string(),
            command: "cargo test".to_string(),
            timeout_ms: 120_000,
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            finished_at: None,
            reported: false,
            output_tail: String::new(),
            consumed: false,
        },
    );
    assert!(app.should_show_task_panel(), "sub-agent opens the panel");
    // Rows live inside the toggle frame — open it for the snapshot.
    app.task_panel.frame_open = true;
    let text = snapshot_text(&mut app, 100, 40);
    assert!(
        text.contains("Tasks (2)"),
        "panel counts sub-agents and bg rows:\n{text}"
    );
    assert!(text.contains("* a"), "sub-agent row missing:\n{text}");
    assert!(
        text.contains("cargo test"),
        "bg row shows the command summary:\n{text}"
    );
    assert!(
        text.contains("running"),
        "bg row shows the status word:\n{text}"
    );

    // Bg-only: the panel opens on the bg row alone.
    let mut app = App::new();
    app.background_tasks.insert(
        "bg_aaaa1111".to_string(),
        BackgroundTaskEntry {
            id: "bg_aaaa1111".to_string(),
            command: "cargo test".to_string(),
            timeout_ms: 120_000,
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            finished_at: None,
            reported: false,
            output_tail: String::new(),
            consumed: false,
        },
    );
    assert!(
        app.should_show_task_panel(),
        "bg task alone opens the panel (it is a row now)"
    );
    app.task_panel.frame_open = true;
    let text = snapshot_text(&mut app, 100, 40);
    assert!(
        text.contains("Tasks (1)"),
        "bg-only panel has one row:\n{text}"
    );
}

#[test]
fn focused_background_row_shows_the_live_tail() {
    let mut app = App::new();
    app.background_tasks.insert(
        "bg_aaaa1111".to_string(),
        BackgroundTaskEntry {
            id: "bg_aaaa1111".to_string(),
            command: "cargo test".to_string(),
            timeout_ms: 120_000,
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            finished_at: None,
            reported: false,
            output_tail: "hello from the build\nstill compiling".to_string(),
            consumed: false,
        },
    );
    app.push_system("root transcript text");
    app.task_panel.focus = FocusTarget::TaskList("bg_aaaa1111".to_string());
    let text = snapshot_text(&mut app, 100, 40);
    assert!(
        text.contains("hello from the build"),
        "live tail missing:\n{text}"
    );
    assert!(
        text.contains("still compiling"),
        "tail shows later lines:\n{text}"
    );
    assert!(
        !text.contains("root transcript text"),
        "the tail view replaces the root transcript:\n{text}"
    );
    assert!(
        text.contains("cargo test"),
        "the tail window title names the task:\n{text}"
    );
}

#[test]
fn queued_strip_renders_above_the_composer() {
    let mut app = App::new();
    app.running = true;
    app.queued_steer.push("two words queued msg".to_string());
    app.queued_cmds.push("/review".to_string());
    let text = snapshot_text(&mut app, 100, 40);
    assert!(
        text.contains("queued -> two words queued msg"),
        "queued message strip missing:\n{text}"
    );
    assert!(
        text.contains("queued after turn -> /review"),
        "held command row missing:\n{text}"
    );

    // Nothing queued: the strip takes no rows at all.
    let mut app = App::new();
    let text = snapshot_text(&mut app, 100, 40);
    assert!(
        !text.contains("queued ->"),
        "empty queue renders no strip:\n{text}"
    );
}

#[test]
fn draw_with_selection_and_context_menu_does_not_panic() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    for i in 0..20 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            detail: None,
            text: format!("line {i}"),
            kind: LineKind::Normal,
            tool_state: None,
        });
    }
    app.output_area = Some((0, 0, 80, 20));
    app.handle_mouse(MouseEventKind::Down(MouseButton::Left), 0, 2, 80, 24);
    // Drag past the row's text end so the head row is selected whole.
    app.handle_mouse(MouseEventKind::Drag(MouseButton::Left), 7, 5, 80, 24);
    app.handle_mouse(MouseEventKind::Down(MouseButton::Right), 10, 5, 80, 24);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    // The selection text round-trips even after rendering.
    assert_eq!(app.selection_text(), "line 2\nline 3\nline 4\nline 5");
}

#[test]
fn draw_and_snapshot_show_mention_popup() {
    let mut app = App::new();
    let root = std::env::temp_dir().join(format!("phimint-render-mention-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.rs"), "x").unwrap();
    app.set_workspace_root(root);

    // Open the picker and type a partial prefix.
    app.handle_key(KeyCode::Char('@'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('m'), KeyModifiers::NONE);

    // Offscreen draw must not panic, and the snapshot shows the popup.
    let text = snapshot_text(&mut app, 100, 40);
    // No framed box, no redundant `@prefix` header row (the composer shows it).
    assert!(
        !text.contains("mention"),
        "popup title should be gone:\n{text}"
    );
    // Rows are prefixed with the selection gutter; the synthetic row is selected.
    assert!(
        text.contains("▸ » m"),
        "selected synthetic row missing:\n{text}"
    );
    assert!(text.contains("📄 main.rs"), "file row missing:\n{text}");
    // The band keeps a bottom separator as the boundary against the transcript.
    assert!(text.contains('─'), "separator missing:\n{text}");
}

#[test]
fn draw_and_snapshot_show_slash_popup() {
    let mut app = App::new();
    app.set_skill_summaries(vec![
        ("review".into(), "Pre-landing PR review".into()),
        ("commit".into(), "Generate a commit message".into()),
        ("code-review".into(), "Review current changes".into()),
    ]);

    // Open the picker by typing `/` at empty composer.
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
    // Type a partial prefix.
    app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);

    let text = snapshot_text(&mut app, 100, 40);
    assert!(
        !text.contains("skills"),
        "popup title should be gone:\n{text}"
    );
    // "review" matches the prefix and is the selected first row.
    assert!(
        text.contains("▸ review"),
        "selected skill row missing:\n{text}"
    );
    assert!(text.contains("Pre-landing"), "description missing:\n{text}");
}

#[test]
fn banner_spans_render_literal_colors_on_top_of_system_style() {
    // The brand channel crosses the seam as compiled data: a `BrandRow` of
    // styled byte-runs where `ShellToken::Literal` paints exactly the product's
    // 24-bit color (and bold when asked). Unspanned bytes fall back to the
    // system base — muted for `push_styled_line` rows. The brand's own palette
    // stops and lockup geometry are the product's business (tested there).
    let backend = TestBackend::new(40, 6);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::new();
    app.push_banner(vec![BrandRow {
        text: "pad█pad".to_string(),
        spans: vec![SpanSpec {
            start: 3,
            len: 3, // "█" is a 3-byte glyph
            style: ShellToken::Literal {
                rgb: (0xff, 0x6a, 0x3d),
                bold: true,
            },
        }],
    }]);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let w = 40usize;
    let cell = |x: usize, y: usize| buf.content()[y * w + x].clone();

    // The literal run paints the product's brand color, bold...
    let painted = cell(3, 0).style();
    assert_eq!(painted.fg, Some(Color::Rgb(0xff, 0x6a, 0x3d)));
    assert!(painted.add_modifier.contains(Modifier::BOLD));
    // ...while unspanned bytes fall back to the system base (muted).
    assert_eq!(cell(0, 0).style().fg, Some(Theme::dark().muted));
    assert_eq!(cell(5, 0).style().fg, Some(Theme::dark().muted));
}

#[test]
fn snapshot_shows_diff_block() {
    let mut app = App::new();
    let args = serde_json::json!({
        "path": "src/main.rs",
        "edits": [
            { "old_text": "fn main() {\n    println!(\"hello\");\n}", "new_text": "fn main() {\n    println!(\"world\");\n}" }
        ]
    });
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ActionStarted {
        session_id: SessionId::new(1),
        tool_name: "edit_file".to_string(),
        args_json: args.to_string(),
        agent_id: None,
        trace_id: None,
    }));

    let text = snapshot_text(&mut app, 100, 40);
    assert!(
        text.contains("┌─ src/main.rs"),
        "diff header missing:\n{text}"
    );
    assert!(text.contains("@@"), "hunk header missing:\n{text}");
    assert!(text.contains("println"), "diff content missing:\n{text}");
    // Verify line numbers are present (hunk header has "1,3")
    assert!(text.contains("-1,3"), "line numbers missing:\n{text}");
}
