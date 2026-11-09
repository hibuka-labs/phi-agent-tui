//! The thinking panel, fold behaviour, and loose-mode thoughts.

use super::super::*;
use super::common::*;
use crate::app::{SubAgentState, SubAgentStatus, TuiEvent};
use crossterm::event::{KeyCode, KeyModifiers};
use phi_agent::{RuntimeEvent, SessionId};
use phi_tui::lines::OutputLine;

#[test]
fn thinking_panel_renders_title_and_stays_out_of_transcript() {
    let mut app = App::new();
    app.push_system("hello");
    let body = "analyze ".repeat(120); // 960 chars → ~320 tok
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: body,
        agent_id: None,
        trace_id: None,
    }));
    let text = snapshot_text(&mut app, 80, 24);
    // Anchor on the panel TITLE ("thinking - …"), not the status bar's
    // "thinking..." spinner line.
    assert!(text.contains("thinking -"), "panel title missing:\n{text}");
    assert!(text.contains("320 tok"), "token estimate missing:\n{text}");
    // The panel must NOT feed the transcript: committed lines unchanged.
    assert_eq!(app.transcript.len(), 1);
}

#[test]
fn thinking_panel_hidden_on_short_terminal() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "analyze ".repeat(120),
        agent_id: None,
        trace_id: None,
    }));
    let text = snapshot_text(&mut app, 80, 10); // height < 12 → no panel
    assert!(!text.contains("thinking -"), "panel should hide:\n{text}");
}

#[test]
fn thinking_panel_follows_focused_child() {
    let mut app = App::new();
    app.sub_agents.insert(
        "root/searcher".to_string(),
        SubAgentState {
            name: "searcher".to_string(),
            status: SubAgentStatus::Running,
            files: Vec::new(),
            started_at: std::time::Instant::now(),
            completed_at: None,
            last_tool_at: std::time::Instant::now(),
            events: Vec::new(),
        },
    );
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "search ".repeat(100),
        agent_id: Some("root/searcher".to_string()),
        trace_id: None,
    }));
    app.task_panel.focus = FocusTarget::TaskList("root/searcher".to_string());
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        text.contains("thinking - root/searcher"),
        "agent name in panel title:\n{text}"
    );
    // Title owns attribution — the body must not repeat the id prefix.
    assert!(
        !text.contains("[root/searcher]"),
        "body must not double-attribute the agent:\n{text}"
    );
}

#[test]
fn committed_thought_folds_and_expands() {
    let mut app = App::new();
    let raw = "line one\nline two\nline three\nline four\nline five";
    app.transcript.push(OutputLine {
        spans: None,
        // Folded thoughts deliberately carry no `original` — `detail.raw` is
        // the single source of truth (`rewrap_output` would explode an
        // `original` on resize and drop `detail`). Construct the production
        // shape flush_thought emits.
        original: None,
        detail: Some(phi_tui::lines::LineDetail::Folded {
            raw: raw.to_string(),
            line_count: 5,
            char_count: raw.chars().count(),
            duration_secs: Some(47),
            meta_head: phi_tui::lines::MetaHead::None,
        }),
        text: "line one".into(),
        kind: LineKind::Thought,
        tool_state: None,
    });
    let text = snapshot_text(&mut app, 80, 24);
    // Line count + estimate (spec: the summary carries a line count and an
    // estimated tok count), then the segment's duration and the expand hint.
    // No leading `>` — that marker belongs to user lines, and a summary
    // starting with `>` reads as something you typed.
    assert!(
        text.contains("thinking - 5 lines - ~16 tok - 47s (Ctrl+O expand)"),
        "summary missing:\n{text}"
    );
    assert!(
        !text.contains("line three"),
        "raw leaked while folded:\n{text}"
    );
    app.show_thoughts = true;
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        text.contains("line three"),
        "expanded text missing:\n{text}"
    );
    assert!(
        !text.contains("thinking - 5 lines"),
        "summary gone when expanded:\n{text}"
    );
}

#[test]
fn thinking_panel_scrolls_away_with_history() {
    // The panel is inline at the flow tail: reviewing history must scroll it
    // away with the content, and follow-bottom must bring it back.
    let mut app = populated_app();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "analyze ".repeat(120),
        agent_id: None,
        trace_id: None,
    }));
    let text = snapshot_text(&mut app, 80, 24);
    // Anchor on the title's token estimate — unique to the panel in this
    // fixture (the status bar says "thinking...", fold summaries carry a
    // different count, and `thinking -` would false-match a future fold).
    assert!(text.contains("~320 tok"), "panel at flow tail:\n{text}");
    for _ in 0..80 {
        app.scroll_up();
    }
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        !text.contains("~320 tok"),
        "panel must scroll away with content:\n{text}"
    );
    app.scroll_to_bottom();
    let text = snapshot_text(&mut app, 80, 24);
    assert!(text.contains("~320 tok"), "panel back at tail:\n{text}");
}

#[test]
fn thinking_panel_hidden_when_focused_child_has_no_thought() {
    // Deviation ① focus-first lock: root is thinking, but the FOCUSED child
    // is idle → the panel must hide. (The draft root-first logic would show
    // the root panel here; that divergence is deliberate — the panel always
    // reports the stream the user is looking at.)
    let mut app = App::new();
    app.sub_agents.insert(
        "root/searcher".to_string(),
        SubAgentState {
            name: "searcher".to_string(),
            status: SubAgentStatus::Running,
            files: Vec::new(),
            started_at: std::time::Instant::now(),
            completed_at: None,
            last_tool_at: std::time::Instant::now(),
            events: Vec::new(),
        },
    );
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "analyze ".repeat(120),
        agent_id: None,
        trace_id: None,
    }));
    app.task_panel.focus = FocusTarget::TaskList("root/searcher".to_string());
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        !text.contains("thinking -"),
        "root panel must not show while an idle child is focused:\n{text}"
    );
}

#[test]
fn thinking_panel_hidden_when_output_pane_too_short() {
    let shot = |w: u16, h: u16| {
        let mut app = App::new();
        app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
            session_id: SessionId::new(1),
            text: "analyze ".repeat(120),
            agent_id: None,
            trace_id: None,
        }));
        snapshot_text(&mut app, w, h)
    };
    // 80x12: output pane = 12 - gap(1) - process(1) - composer(3) -
    // status(1) = 6 rows — the box alone wouldn't fit; the fit guard hides the
    // panel so history survives.
    let text = shot(80, 12);
    assert!(
        !text.contains("thinking -"),
        "panel must hide, not occlude:\n{text}"
    );
    // 80x16: output pane = 16 - 6 = 10 = box(8) + history(2) — exact boundary
    // shows.
    let text = shot(80, 16);
    assert!(
        text.contains("thinking -"),
        "boundary (8+2) must show:\n{text}"
    );
    // 80x20: output pane = 14 ≥ box(8) + history(2) — panel shows.
    let text = shot(80, 20);
    assert!(
        text.contains("thinking -"),
        "panel should fit with history rows:\n{text}"
    );
}

#[test]
fn thinking_panel_blank_when_cut_by_window_edge() {
    // The panel is one unit: a box cut by the window edge is left blank —
    // never half-overlayed at the wrong height.
    let mut app = populated_app();
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "analyze ".repeat(120),
        agent_id: None,
        trace_id: None,
    }));
    let text = snapshot_text(&mut app, 80, 24);
    assert!(text.contains("thinking -"), "full panel at tail:\n{text}");
    assert!(text.contains("analyze"), "panel body at tail:\n{text}");
    app.scroll_wheel_up(); // 1-row cut → partial → blank
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        !text.contains("thinking -") && !text.contains("analyze"),
        "cut panel must be blank (no title, no body, no half box):\n{text}"
    );
}

#[test]
fn thinking_panel_anchors_row_position_below_history() {
    // The box sits exactly where its placeholder rows land: directly under the
    // committed history. Three separate `push_system` calls are three system
    // *blocks* (design §1 — a block is one message, not one kind), so they
    // interleave with render-time spacers and the title row lands at 5.
    // 80x19 gives the output pane the exact 13 rows history + box need
    // (19 - 6 chrome rows).
    let mut app = App::new();
    app.push_system("one");
    app.push_system("two");
    app.push_system("three");
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "analyze ".repeat(120),
        agent_id: None,
        trace_id: None,
    }));
    let text = snapshot_text(&mut app, 80, 19);
    let rows: Vec<&str> = text.lines().collect();
    // Block rhythm: content, spacer, content, spacer, content, then the panel.
    assert_eq!(rows.first().map(|r| r.trim()), Some("one"), "\n{text}");
    assert_eq!(rows.get(1).map(|r| r.trim()), Some(""), "\n{text}");
    assert_eq!(rows.get(2).map(|r| r.trim()), Some("two"), "\n{text}");
    assert_eq!(rows.get(3).map(|r| r.trim()), Some(""), "\n{text}");
    assert_eq!(rows.get(4).map(|r| r.trim()), Some("three"), "\n{text}");

    let title = rows
        .iter()
        .position(|r| r.contains('╭') && r.contains("thinking -"))
        .unwrap_or_else(|| panic!("panel title row missing:\n{text}"));
    assert_eq!(
        title, 5,
        "panel title row must sit right below history, got row {title}:\n{text}"
    );
}

#[test]
fn history_position_holds_when_thought_opens() {
    // Scroll anchoring: opening the panel's 8 placeholder rows at the tail
    // must not yank a mid-read history view toward the tail.
    let mut app = populated_app();
    let _ = snapshot_text(&mut app, 80, 24); // establish viewport sizes
    for _ in 0..3 {
        app.scroll_up();
    }
    let before = snapshot_text(&mut app, 80, 24);
    let head = before.lines().next().unwrap_or_default().to_string();
    assert!(
        !head.is_empty(),
        "scrolled history should show content:\n{before}"
    );
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "analyze ".repeat(120),
        agent_id: None,
        trace_id: None,
    }));
    let after = snapshot_text(&mut app, 80, 24);
    let head_after = after.lines().next().unwrap_or_default();
    assert_eq!(
        head, head_after,
        "history head must not jump when the thought panel opens:\n{after}"
    );
}

#[test]
fn expanded_thought_rewraps_at_current_width() {
    // Ctrl+O expansion re-wraps `detail.raw` at the live width — a 200-char
    // run must split into rows, not hard-clip or overflow one row.
    let mut app = App::new();
    let long = "x".repeat(200); // hard-wraps into 3 rows at width 80
    let raw = format!("line one\n{long}\nline three");
    let char_count = raw.chars().count();
    app.transcript.push(OutputLine {
        spans: None,
        // Production folded shape (see committed_thought_folds_and_expands).
        original: None,
        detail: Some(phi_tui::lines::LineDetail::Folded {
            raw: raw.clone(),
            line_count: 3,
            char_count,
            duration_secs: None,
            meta_head: phi_tui::lines::MetaHead::None,
        }),
        text: "line one".into(),
        kind: LineKind::Thought,
        tool_state: None,
    });
    app.show_thoughts = true;
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        text.contains(&"x".repeat(80)),
        "long line must hard-wrap:\n{text}"
    );
    assert!(
        !text.contains(&"x".repeat(200)),
        "un-wrapped 200-char run must not survive:\n{text}"
    );
    assert!(
        text.contains("line three"),
        "content after the long line:\n{text}"
    );
}

#[test]
fn history_head_survives_fold_reflow_above_viewport() {
    // Ctrl+O reflows the MIDDLE of the flow: a folded thought above the window
    // head grows from 1 summary row to 8. The reader's place must hold by
    // CONTENT — holding the raw head index would silently swap in content
    // from above (the tail-only anchoring gap).
    let mut app = App::new();
    let raw = (0..8)
        .map(|i| format!("thought line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.transcript.push(OutputLine {
        spans: None,
        // Production folded shape (see committed_thought_folds_and_expands).
        original: None,
        detail: Some(phi_tui::lines::LineDetail::Folded {
            raw: raw.clone(),
            line_count: 8,
            char_count: raw.chars().count(),
            duration_secs: None,
            meta_head: phi_tui::lines::MetaHead::None,
        }),
        text: "thought line 0".into(),
        kind: LineKind::Thought,
        tool_state: None,
    });
    for i in 0..60 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            detail: None,
            text: format!("filler row {i}"),
            kind: LineKind::Normal,
            tool_state: None,
        });
    }
    let _ = snapshot_text(&mut app, 80, 24); // establish viewport sizes
    assert!(app.scroll_up(), "fixture must be scrollable");
    let before = snapshot_text(&mut app, 80, 24);
    let head = before.lines().next().unwrap_or_default().to_string();
    assert!(
        head.contains("filler row"),
        "head must sit below the folded thought:\n{before}"
    );
    assert!(
        !before.contains("thought line 7"),
        "raw hidden while folded"
    );

    app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL); // +7 rows above head

    let after = snapshot_text(&mut app, 80, 24);
    assert_eq!(
        head,
        after.lines().next().unwrap_or_default(),
        "mid-flow fold reflow must not move the reader:\n{after}"
    );
    // Positive control: the reflow really grew the flow — otherwise the
    // equality above would pass vacuously on a no-op toggle.
    app.viewport.scroll_to_top();
    let top = snapshot_text(&mut app, 80, 24);
    assert!(
        top.contains("thought line 7"),
        "expansion must be visible from the top:\n{top}"
    );
}

/// Fixture for the two expanded-thought scroll tests: one long thought (40
/// visual rows when expanded — taller than the ~20-row output pane) followed
/// by a short answer tail. Expanded thinking is ONE output row, so the window
/// head can sit anywhere inside that 40-row block.
fn expanded_thought_with_tail() -> App {
    let mut app = App::new();
    let raw = (0..40)
        .map(|i| format!("thought line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.transcript.push(OutputLine {
        spans: None,
        original: None,
        detail: Some(phi_tui::lines::LineDetail::Folded {
            raw: raw.clone(),
            line_count: 40,
            char_count: raw.chars().count(),
            duration_secs: None,
            meta_head: phi_tui::lines::MetaHead::None,
        }),
        text: "thought line 0".into(),
        kind: LineKind::Thought,
        tool_state: None,
    });
    for i in 0..6 {
        app.transcript.push(OutputLine {
            spans: None,
            original: None,
            detail: None,
            text: format!("filler row {i}"),
            kind: LineKind::Normal,
            tool_state: None,
        });
    }
    let _ = snapshot_text(&mut app, 80, 24); // establish viewport sizes
    app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL); // expand
    app
}

#[test]
fn one_row_scroll_up_off_bottom_stays_inside_expanded_thought() {
    // Under follow-bottom the window head lands mid-block (max_offset sits
    // inside the 40-row thought). A one-row scroll up must move exactly one
    // row — resolving the head to the block's first row flings the view a
    // whole thought upward (session 20260923: "can't scroll to the bottom").
    let mut app = expanded_thought_with_tail();

    let bottom = snapshot_text(&mut app, 80, 24);
    let head = bottom.lines().next().unwrap_or_default().to_string();
    let n: usize = head
        .trim()
        .strip_prefix("thought line ")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("bottom head must sit mid-thought:\n{bottom}"));
    assert!(
        n >= 1,
        "bottom head must leave room for one row up:\n{bottom}"
    );
    assert!(
        bottom.contains("filler row 5"),
        "bottom must show the flow tail:\n{bottom}"
    );

    app.viewport.scroll_up(1);
    let after = snapshot_text(&mut app, 80, 24);
    assert_eq!(
        after.lines().next().unwrap_or_default().trim(),
        format!("thought line {}", n - 1),
        "one row up must move exactly one row (no block-top snap):\n{after}"
    );
}

#[test]
fn wheel_down_through_expanded_thought_reaches_tail() {
    // The user's literal gesture (session 20260923): wheel up off the bottom,
    // then wheel back down to the answer. The anchor re-resolves every frame,
    // so the test must render between ticks like the real event loop —
    // otherwise the scroll methods alone accumulate and mask the bug.
    let mut app = expanded_thought_with_tail();

    app.scroll_wheel_up();
    let _ = snapshot_text(&mut app, 80, 24);
    for _ in 0..80 {
        app.scroll_wheel_down();
        let _ = snapshot_text(&mut app, 80, 24);
    }

    let end = snapshot_text(&mut app, 80, 24);
    assert!(
        end.contains("filler row 5"),
        "wheel-down must reach the flow tail:\n{end}"
    );
}

#[test]
fn loose_mode_streams_thought_inline_without_box() {
    // Ctrl+O ON ("loose"): the in-flight thought prints into the flow like
    // any other content — no TailPanel box, no fold chrome. History thoughts
    // expand via the same toggle (committed_thought_folds_and_expands).
    let mut app = App::new();
    app.push_system("hello");
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "analyze ".repeat(50),
        agent_id: None,
        trace_id: None,
    }));
    app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL); // loose
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        text.contains("analyze"),
        "thought streams into the flow:\n{text}"
    );
    // Panel absence is anchored on its title row (`thinking -`): the composer
    // is a rounded box too, so the bare `╭` glyph proves nothing here.
    assert!(
        !text.contains("thinking -"),
        "no panel box/title in loose mode:\n{text}"
    );
}

#[test]
fn loose_mode_child_thought_carries_agent_prefix() {
    // Boxed mode names the author in the panel title; loose mode has no
    // title, so attribution rides the first streamed row (tool-line style).
    let mut app = App::new();
    app.sub_agents.insert(
        "root/searcher".to_string(),
        SubAgentState {
            name: "searcher".to_string(),
            status: SubAgentStatus::Running,
            files: Vec::new(),
            started_at: std::time::Instant::now(),
            completed_at: None,
            last_tool_at: std::time::Instant::now(),
            events: Vec::new(),
        },
    );
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "search ".repeat(50),
        agent_id: Some("root/searcher".to_string()),
        trace_id: None,
    }));
    app.task_panel.focus = FocusTarget::TaskList("root/searcher".to_string());
    app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL); // loose
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        text.contains("[root/searcher]"),
        "author on the first row:\n{text}"
    );
    assert!(text.contains("search"), "thought body streams:\n{text}");
}

#[test]
fn ctrl_o_mid_thought_swaps_box_and_stream() {
    // One toggle, two coupled flips: the in-flight thought (box <-> inline
    // stream) and history thoughts (fold <-> expand) change together.
    let mut app = App::new();
    app.push_system("hello");
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "analyze ".repeat(50),
        agent_id: None,
        trace_id: None,
    }));
    let boxed = snapshot_text(&mut app, 80, 24);
    assert!(boxed.contains("thinking -"), "default is boxed:\n{boxed}");
    app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL);
    let loose = snapshot_text(&mut app, 80, 24);
    assert!(
        loose.contains("analyze") && !loose.contains("thinking -"),
        "loose streams without the panel:\n{loose}"
    );
    app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL);
    let boxed = snapshot_text(&mut app, 80, 24);
    assert!(
        boxed.contains("thinking -"),
        "box comes back around the same thought:\n{boxed}"
    );
}

#[test]
fn loose_commit_keeps_agent_prefix_after_flush() {
    // The flush seam: loose rows carry `[{agent}] ` and `detail.raw` is baked
    // with the same prefix, so when the segment commits and renders expanded
    // the author does not vanish mid-stream.
    let mut app = App::new();
    app.sub_agents.insert(
        "root/searcher".to_string(),
        SubAgentState {
            name: "searcher".to_string(),
            status: SubAgentStatus::Running,
            files: Vec::new(),
            started_at: std::time::Instant::now(),
            completed_at: None,
            last_tool_at: std::time::Instant::now(),
            events: Vec::new(),
        },
    );
    app.task_panel.focus = FocusTarget::TaskList("root/searcher".to_string());
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: "search ".repeat(50),
        agent_id: Some("root/searcher".to_string()),
        trace_id: None,
    }));
    app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL); // loose BEFORE commit
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::TextDelta {
        session_id: SessionId::new(1),
        text: "answer ".repeat(10),
        agent_id: Some("root/searcher".to_string()),
        trace_id: None,
    })); // kind flip flushes the thought
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        text.contains("[root/searcher]"),
        "expanded committed thought keeps the author at the seam:\n{text}"
    );
    assert!(
        text.contains("search"),
        "thought body still visible (expanded):\n{text}"
    );
}
