//! The ask_user choice popup.

use super::super::*;

// ── ask_user choice popup (the centered modal) ─────────────────────────────

fn choice_rows(spec: &[(&str, Option<&str>)]) -> Vec<crate::picker::ChoiceRow> {
    spec.iter()
        .map(|(label, note)| crate::picker::ChoiceRow {
            label: (*label).to_string(),
            note: note.map(|n| n.to_string()),
        })
        .collect()
}

fn choice_to_text(
    width: u16,
    height: u16,
    question: &str,
    rows: &[crate::picker::ChoiceRow],
    selection: usize,
) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| draw_choice_popup(f, question, rows, selection, Theme::dark()))
        .unwrap();
    buffer_to_text(terminal.backend().buffer())
}

fn push_ask(
    app: &mut App,
    question: &str,
    labels: &[&str],
) -> tokio::sync::oneshot::Receiver<crate::interaction::AskAnswer> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.ask_queue.push_back(crate::interaction::AskItem {
        question: question.to_string(),
        options: labels
            .iter()
            .map(|label| crate::interaction::AskOption {
                id: label.to_string(),
                label: label.to_string(),
                note: None,
            })
            .collect(),
        answer_tx: tx,
    });
    rx
}

#[test]
fn a_question_picker_shows_the_question_options_and_the_way_out() {
    let text = choice_to_text(
        48,
        12,
        "先做哪个？",
        &choice_rows(&[("搜索", Some("先打通找文件")), ("登录", None)]),
        0,
    );
    assert!(text.contains("先做哪个？"), "question missing:\n{text}");
    assert!(text.contains("搜索") && text.contains("登录"), "{text}");
    assert!(text.contains("先打通找文件"), "note missing:\n{text}");
    assert!(
        text.contains("Esc skip"),
        "the way out must be on screen:\n{text}"
    );
}

#[test]
fn the_first_option_is_the_one_under_the_cursor() {
    // `> ` is the cursor column, so it appears exactly once and on the row
    // the selection names — Enter on a fresh popup picks the first candidate.
    let text = choice_to_text(
        48,
        12,
        "哪个？",
        &choice_rows(&[("搜索", None), ("登录", None)]),
        0,
    );
    assert_eq!(text.matches("> ").count(), 1, "{text}");
    assert!(
        text.contains("> 搜索"),
        "the cursor must start on the first option:\n{text}"
    );
}

#[test]
fn the_cursor_follows_the_selection_down_the_list() {
    let text = choice_to_text(
        48,
        12,
        "哪个？",
        &choice_rows(&[("搜索", None), ("登录", None)]),
        1,
    );
    assert!(
        text.contains("> 登录"),
        "the cursor must move with the selection:\n{text}"
    );
}

#[test]
fn a_long_question_does_not_crowd_the_options_off_the_frame() {
    let question = "这一版的搜索结果页，列表和筛选栏的布局先按哪种走：左侧筛选常驻、顶部筛选可折叠、还是只留搜索框？";
    let text = choice_to_text(
        30,
        8,
        question,
        &choice_rows(&[("常驻", None), ("折叠", None)]),
        0,
    );
    assert!(text.contains("常驻") && text.contains("折叠"), "{text}");
    assert!(text.contains("Esc skip"), "{text}");
}

#[test]
fn the_question_keeps_its_last_character_when_it_is_the_widest_content() {
    // The question paragraph pads 1 column each side. Budgeting its raw width
    // left the box 2 columns short and the trailing 「？」 clipped — seen live
    // where the question was the widest row in the popup.
    let question = "动手前的概念选择：工单管理后台首页的视觉方向，先从哪一头入手？";
    let text = choice_to_text(
        80,
        14,
        question,
        &choice_rows(&[("布局", Some("先定信息架构")), ("配色", None)]),
        0,
    );
    assert!(
        text.contains(question),
        "the whole question must fit on its row:\n{text}"
    );
}

#[test]
fn an_empty_option_list_draws_nothing() {
    // A question with no options has nothing to pick — the popup must not
    // paint an empty window over the transcript.
    let text = choice_to_text(48, 12, "哪个？", &[], 0);
    assert!(
        !text.contains("Choose"),
        "no window for a question nobody can answer:\n{text}"
    );
    assert!(
        !text.contains("哪个？"),
        "nothing at all should render:\n{text}"
    );
}

#[test]
fn the_ask_popup_is_a_window_over_the_transcript_not_a_takeover() {
    // The whole point of the redesign: the transcript stays on screen
    // underneath, and the question is a small centered box — never a blank
    // alternate screen of its own.
    let mut app = App::new();
    app.push_system("transcript line that must survive");
    push_ask(&mut app, "先做哪个？", &["搜索", "登录"]);
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        text.contains("transcript line that must survive"),
        "the transcript must stay visible behind the popup:\n{text}"
    );
    assert!(text.contains("先做哪个？"), "question missing:\n{text}");
    assert!(text.contains("Choose"), "window title missing:\n{text}");
    assert!(text.contains("Esc skip"), "way out missing:\n{text}");
}

/// protects: the "Chat about this" pseudo-row renders at the option tail with
/// its note — the third exit is always on screen.
/// fails_when: the row is missing, not last, or renders without its note.
/// no_claim: its selection/Enter behavior (app cards) or the clarify face.
#[test]
fn the_chat_about_this_row_is_the_last_row() {
    let mut app = App::new();
    push_ask(&mut app, "先做哪个？", &["搜索", "登录"]);
    let text = snapshot_text(&mut app, 80, 24);
    assert!(
        text.contains("Chat about this"),
        "pseudo-row missing:\n{text}"
    );
    assert!(
        text.contains("type a question instead of picking"),
        "its note must say what it is for:\n{text}"
    );
    let options_end = text.find("登录").expect("options rendered");
    let chat_at = text.find("Chat about this").unwrap();
    assert!(
        chat_at > options_end,
        "the pseudo-row must come after the options:\n{text}"
    );
}

/// protects: a 0-option host-call ask draws its window (question + the
/// pseudo-row) instead of hanging invisible — the fix for the empty-rows
/// early return that left those asks unanswerable on screen.
/// fails_when: zero options paint nothing, or the question/way out is gone.
/// no_claim: the keyboard path (app cards) or non-zero option lists.
#[test]
fn a_zero_option_ask_still_draws_its_window() {
    let mut app = App::new();
    push_ask(&mut app, "free form?", &[]);
    let text = snapshot_text(&mut app, 80, 24);
    assert!(text.contains("Choose"), "no window at all:\n{text}");
    assert!(text.contains("free form?"), "question missing:\n{text}");
    assert!(
        text.contains("Chat about this"),
        "the pseudo-row is the only row and must render:\n{text}"
    );
}

/// protects: the clarify face — question + one-line input (its own buffer,
/// cursor at the end) + the key legend, in place of the option list.
/// fails_when: the typed text or the legend is missing, or the option list
/// (pseudo-row included) still paints over the input.
/// no_claim: the input's key handling (app cards) or text longer than the box.
#[test]
fn the_clarify_face_swaps_the_list_for_an_input() {
    let mut app = App::new();
    push_ask(&mut app, "先做哪个？", &["搜索", "登录"]);
    app.ask_clarify_open();
    for c in "why not both?".chars() {
        app.ask_clarify_push(c);
    }
    let text = snapshot_text(&mut app, 80, 24);
    assert!(text.contains("先做哪个？"), "question missing:\n{text}");
    assert!(
        text.contains("> why not both?"),
        "input row missing:\n{text}"
    );
    assert!(
        text.contains("Enter send") && text.contains("Esc back"),
        "legend missing:\n{text}"
    );
    assert!(
        !text.contains("Chat about this"),
        "the option list must be gone in the input face:\n{text}"
    );
    assert!(!text.contains("搜索"), "options must not paint:\n{text}");
}

#[test]
fn an_approval_in_front_hides_the_question_behind_it() {
    // One modal at a time — the approval's key rules own the keyboard, so the
    // question must not also paint its own window.
    let mut app = App::new();
    push_ask(&mut app, "先做哪个？", &["搜索", "登录"]);
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
        text.contains("approval"),
        "approval window missing:\n{text}"
    );
    assert!(
        !text.contains("Choose"),
        "the question behind must not paint a second window:\n{text}"
    );
}
