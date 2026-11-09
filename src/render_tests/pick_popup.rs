//! The product pick modal (the `/model` chooser): rows, filter face, legend.

use super::super::*;

fn pick_rows(spec: &[(&str, Option<&str>, bool)]) -> Vec<crate::interaction::PickRow> {
    spec.iter()
        .map(|(label, note, current)| crate::interaction::PickRow {
            id: (*label).to_string(),
            label: (*label).to_string(),
            note: note.map(|n| n.to_string()),
            current: *current,
        })
        .collect()
}

fn pick_to_terminal(
    width: u16,
    height: u16,
    rows: &[crate::interaction::PickRow],
    visible: &[usize],
    filter: &str,
    selection: usize,
) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| {
            draw_pick_popup(
                f,
                PickFrame {
                    title: "Model",
                    rows,
                    visible,
                    filter,
                    selection,
                    footer: " Enter set  Esc close ",
                    no_match: "no match - Enter uses the typed name",
                },
                Theme::dark(),
            )
        })
        .unwrap();
    terminal
}

fn pick_to_text(
    width: u16,
    height: u16,
    rows: &[crate::interaction::PickRow],
    visible: &[usize],
    filter: &str,
    selection: usize,
) -> String {
    let terminal = pick_to_terminal(width, height, rows, visible, filter, selection);
    buffer_to_text(terminal.backend().buffer())
}

/// protects: the modal shows the product's title, its rows verbatim (labels
/// carry the product's tier tags), notes under labels, and the way out.
/// fails_when: a row, its note, or the Esc legend clips off the frame.
/// no_claim: the product's label/tag wording.
#[test]
fn the_modal_shows_the_title_rows_and_the_way_out() {
    let rows = pick_rows(&[
        ("claude-opus-5-5 [main]", Some("current"), true),
        ("deepseek-chat [lite]", None, false),
    ]);
    let text = pick_to_text(48, 12, &rows, &[0, 1], "", 0);
    assert!(text.contains("Model"), "title missing:\n{text}");
    assert!(text.contains("claude-opus-5-5 [main]"), "{text}");
    assert!(text.contains("deepseek-chat [lite]"), "{text}");
    assert!(text.contains("current"), "note missing:\n{text}");
    assert!(
        text.contains("Esc close"),
        "the way out must be on screen:\n{text}"
    );
}

/// protects: the filter face owns the top row ("> " + typed prefix) and an
/// empty match list shows the no-match hint instead of rows — the literal
/// fallback is discoverable at the moment it applies. fails_when: the typed
/// prefix is not visible, or an empty list renders as a blank hole.
/// no_claim: the matching policy behind `visible`.
#[test]
fn the_filter_row_is_on_top_and_no_match_says_enter_uses_the_typed_name() {
    let rows = pick_rows(&[("claude-opus-5-5", None, false)]);
    let text = pick_to_text(48, 12, &rows, &[], "gpt-9", 0);
    assert!(text.contains("> gpt-9"), "filter row missing:\n{text}");
    assert!(
        text.contains("no match - Enter uses the typed name"),
        "{text}"
    );
    assert!(
        !text.contains("claude-opus-5-5"),
        "rows hide on no match:\n{text}"
    );

    // The filter narrows the rendered rows to the match list.
    let rows = pick_rows(&[("alpha", None, false), ("beta", None, false)]);
    let text = pick_to_text(48, 12, &rows, &[1], "be", 0);
    assert!(text.contains("beta"), "{text}");
    assert!(
        !text.contains("alpha"),
        "filtered rows must not render:\n{text}"
    );
}

/// protects: the product's current-row mark survives rendering as a bold
/// label even when the cursor sits elsewhere. fails_when: the current row
/// renders plain without the cursor on it. no_claim: which row the product
/// marks current.
#[test]
fn the_current_row_is_bold_without_the_cursor() {
    let rows = pick_rows(&[
        ("alpha-model", None, false),
        ("beta-model", Some("current"), true),
        ("gamma-model", None, false),
    ]);
    let terminal = pick_to_terminal(48, 12, &rows, &[0, 1, 2], "", 0);
    let buf = terminal.backend().buffer().clone();
    let text = buffer_to_text(&buf);
    let y = text
        .lines()
        .position(|line| line.contains("beta-model"))
        .expect("the current row renders") as u16;
    let bold = (0..buf.area.width).any(|x| {
        buf.cell((x, y))
            .is_some_and(|cell| cell.modifier.contains(Modifier::BOLD))
    });
    assert!(bold, "the current row must render bold:\n{text}");

    // The plain row — not the cursor row, which wears the selection's own
    // bold — must render without any emphasis.
    let x = text
        .lines()
        .position(|line| line.contains("gamma-model"))
        .expect("the plain row renders") as u16;
    let row_bold = (0..buf.area.width).any(|i| {
        buf.cell((i, x))
            .is_some_and(|cell| cell.modifier.contains(Modifier::BOLD))
    });
    assert!(!row_bold, "a non-current row renders plain:\n{text}");
}

/// protects: when the list outgrows the box, the window follows the cursor —
/// Enter can never commit a row the user cannot see. fails_when: the cursor
/// row clips off a crowded frame. no_claim: the scrolling aesthetics.
#[test]
fn the_cursor_row_survives_a_list_taller_than_the_box() {
    let rows: Vec<crate::interaction::PickRow> = (0..12)
        .map(|i| crate::interaction::PickRow {
            id: format!("model-{i:02}"),
            label: format!("model-{i:02}"),
            note: None,
            current: false,
        })
        .collect();
    let visible: Vec<usize> = (0..rows.len()).collect();
    let text = pick_to_text(30, 8, &rows, &visible, "", 11);
    assert!(
        text.contains("> model-11"),
        "the cursor row must stay visible:\n{text}"
    );
}
