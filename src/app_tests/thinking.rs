//! Thought segment timers and the show-thoughts toggle.

use super::super::*;
use super::common::*;
use phi_agent::SessionId;

#[test]
fn thought_delta_opens_thinking_segment() {
    let mut app = App::new();
    assert!(!app.thinking_since.contains_key(""));
    app.handle_event(TuiEvent::Runtime(thought("hmm ")));
    assert!(
        app.thinking_since.contains_key(""),
        "first delta opens a segment"
    );
}

#[test]
fn flush_pending_clears_thinking_segment() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(thought(&"x ".repeat(200))));
    assert!(app.thinking_since.contains_key(""));
    app.flush_pending();
    assert!(!app.thinking_since.contains_key(""));
}

fn thought_as(s: &str, agent: &str) -> RuntimeEvent {
    RuntimeEvent::ThoughtDelta {
        session_id: SessionId::new(1),
        text: s.to_string(),
        agent_id: Some(agent.to_string()),
        trace_id: None,
    }
}

#[test]
fn second_thought_delta_keeps_segment_start() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(thought("a ")));
    let t0 = app.thinking_since.get("").copied();
    app.handle_event(TuiEvent::Runtime(thought("b ")));
    assert_eq!(
        app.thinking_since.get("").copied(),
        t0,
        "same segment keeps its start"
    );
}

#[test]
fn text_delta_retires_thought_timer() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(thought(&"x ".repeat(200))));
    assert!(app.thinking_since.contains_key(""));
    app.handle_event(TuiEvent::Runtime(text("answer ")));
    assert!(
        !app.thinking_since.contains_key(""),
        "implicit flush retires the timer"
    );
}

#[test]
fn child_flush_keeps_other_children_timers() {
    let mut app = App::new();
    app.handle_event(TuiEvent::Runtime(thought_as("A thinking ", "root/a")));
    app.handle_event(TuiEvent::Runtime(thought_as("B thinking ", "root/b")));
    assert!(app.thinking_since.contains_key("root/a"));
    assert!(app.thinking_since.contains_key("root/b"));
    app.flush_child_stream("root/a");
    assert!(!app.thinking_since.contains_key("root/a"));
    assert!(
        app.thinking_since.contains_key("root/b"),
        "B's timer survives A's flush"
    );
}

#[test]
fn ctrl_o_toggles_show_thoughts() {
    let mut app = App::new();
    assert!(!app.show_thoughts);
    app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert!(app.show_thoughts);
    app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert!(!app.show_thoughts);
    // Plain 'o' still types into the composer.
    app.handle_key(KeyCode::Char('o'), KeyModifiers::NONE);
    assert!(!app.show_thoughts);
    assert_eq!(app.composer.text(), "o");
}

#[test]
fn thought_agent_change_restarts_timer() {
    // `push_thought` implicitly flushes the previous segment on a kind/agent
    // flip. The new segment must re-arm the panel timer (`!was` alone keeps
    // the old `Instant` and overstates elapsed).
    let mut app = App::new();
    let thought = |agent: Option<&str>| {
        TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
            session_id: SessionId::new(1),
            text: "think ".repeat(50),
            agent_id: agent.map(String::from),
            trace_id: None,
        })
    };
    app.handle_event(thought(Some(""))); // routes to the root stream, tag Some("")
    let first = *app.thinking_since.get("").expect("timer armed");
    let n = app.transcript.len();
    app.handle_event(thought(None)); // same stream, different tag → implicit flush
    assert!(
        app.transcript.len() > n,
        "agent-change flush must commit the old segment"
    );
    assert!(
        app.transcript
            .output
            .iter()
            .any(|l| l.kind == LineKind::Thought),
        "old segment commits as a thought line"
    );
    let second = *app.thinking_since.get("").expect("timer re-armed");
    assert!(
        second > first,
        "re-arm must install a FRESH Instant — a stale one overstates elapsed"
    );
}

#[test]
fn child_thought_text_thought_rearms_timer() {
    // Kind-flip through prose: thought → text → thought on one child stream.
    // The implicit flush retires the timer; the new segment must re-arm it.
    let mut app = App::new();
    let thought = || {
        TuiEvent::Runtime(RuntimeEvent::ThoughtDelta {
            session_id: SessionId::new(1),
            text: "think ".repeat(50),
            agent_id: Some("root/a".to_string()),
            trace_id: None,
        })
    };
    app.handle_event(thought());
    let first = *app.thinking_since.get("root/a").expect("child timer armed");
    app.handle_event(TuiEvent::Runtime(RuntimeEvent::TextDelta {
        session_id: SessionId::new(1),
        text: "prose ".repeat(20),
        agent_id: Some("root/a".to_string()),
        trace_id: None,
    }));
    assert!(
        !app.thinking_since.contains_key("root/a"),
        "prose retires the child's thought timer"
    );
    app.handle_event(thought());
    let second = *app
        .thinking_since
        .get("root/a")
        .expect("child timer re-armed");
    assert!(second > first, "new segment must get a fresh timer");
}

#[test]
fn popup_style_defaults_to_frameless_and_is_settable() {
    use phi_tui::popup_list::{PopupStyle, WidthSpec};
    let mut app = App::new();
    assert!(
        !app.popup_style().frame,
        "default popup style must be frameless"
    );
    assert_eq!(app.popup_style().width, WidthSpec::Fill);

    app.set_popup_style(PopupStyle::framed(64, 9));
    assert!(app.popup_style().frame);
    assert_eq!(app.popup_style().width, WidthSpec::Fixed(64));
}
